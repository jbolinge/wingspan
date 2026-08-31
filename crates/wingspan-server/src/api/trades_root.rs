//! `GET`/`PUT`/`DELETE /api/v1/trades/root` — the folder the book is read
//! from, changed from the status bar instead of from `wingspan.toml`.
//!
//! The three verbs share one body (`docs/API.md`) so the SPA can render the
//! result of a change with the same code that renders the initial state.
//! Everything that decides *which* folder wins lives in
//! [`crate::state`]; this module is only the wire.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use super::json::Json as JsonBody;

use crate::dto;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SetRootError};

pub async fn get(State(state): State<Arc<AppState>>) -> Json<dto::TradesRootDto> {
    Json(body(&state))
}

/// Pick a folder: validated, persisted, and applied like a restart.
pub async fn put(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradesRootRequest>,
) -> ApiResult<Json<dto::TradesRootDto>> {
    state
        .set_root(&request.root)
        .await
        .map_err(|err| problem(&err))?;
    Ok(Json(body(&state)))
}

/// Drop the choice and go back to what the config (or `--demo`) says.
pub async fn delete(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesRootDto>> {
    state.clear_root().await.map_err(|err| problem(&err))?;
    Ok(Json(body(&state)))
}

/// The one body all three verbs answer with — and, with its `uploaded` field
/// filled in, the one `POST /trades/upload` answers with too.
pub(crate) fn body(state: &AppState) -> dto::TradesRootDto {
    let active = state.root.load();
    let book = state.book();
    dto::TradesRootDto {
        root: active.root.root.clone(),
        source: active.source,
        config_root: state.config.trades.resolved_root(),
        open_trades: book.book.open_trades().count(),
        closed_trades: book.book.closed_trades().count(),
        uploaded_at: active.uploaded_at,
        uploaded: None,
    }
}

/// The statuses `docs/API.md` freezes: the discovery sentence is the 422's
/// `detail`, "nothing to fall back to" is a 404, and a state directory we
/// cannot write is ours to own.
pub(crate) fn problem(err: &SetRootError) -> ApiError {
    let detail = err.to_string();
    match err {
        SetRootError::Discovery(_) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-trades-root",
            detail,
        )
        .titled("Not a trades folder"),
        SetRootError::NoFallback => ApiError::not_found(format!(
            "{detail}; keep the folder you picked, or add a root to wingspan.toml"
        )),
        SetRootError::Persist { .. } | SetRootError::Startup(_) => ApiError::internal(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingspan_core::discovery::{DirNames, DiscoveryError};

    #[test]
    fn a_bad_folder_is_a_422_carrying_the_discovery_sentence() {
        let err = SetRootError::Discovery(DiscoveryError::NoTradesDirs {
            start: "/tmp/nope".into(),
            names: DirNames::default(),
        });
        let sentence = err.to_string();
        let problem = problem(&err);
        assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem.detail, sentence);
        assert!(problem.detail.contains("Open Trades"), "{problem}");
    }

    #[test]
    fn nothing_to_fall_back_to_is_a_404_that_says_what_to_do() {
        let problem = problem(&SetRootError::NoFallback);
        assert_eq!(problem.status, StatusCode::NOT_FOUND);
        assert!(problem.detail.contains("wingspan.toml"), "{problem}");
    }

    #[test]
    fn an_unwritable_state_dir_is_ours_not_the_users_fault() {
        let problem = problem(&SetRootError::Persist {
            path: "/read/only/trades_root.json".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        });
        assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(problem.detail.contains("trades_root.json"), "{problem}");
    }
}
