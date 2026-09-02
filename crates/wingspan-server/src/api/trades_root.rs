//! `GET`/`DELETE /api/v1/trades/root` — which book is loaded, and clearing it.
//!
//! Trade files are never kept on this machine as a matter of policy: they are
//! uploaded from the browser (`POST /api/v1/trades/upload`), analysed, and
//! removed again. So there are two verbs left. `GET` says what is loaded and
//! how it got here; `DELETE` is the Clear button — it deletes the uploaded
//! copy and everything the server derived from it, then falls back to
//! whatever `[trades] root` (or `--demo`) says, or to no book at all.
//!
//! Both share one body (`docs/API.md`), which is also what `POST
//! /trades/upload` answers with, so the SPA renders every one of them with
//! the same code. Everything that decides *which* book wins lives in
//! [`crate::state`]; this module is only the wire.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use crate::dto;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SetRootError};

pub async fn get(State(state): State<Arc<AppState>>) -> Json<dto::TradesRootDto> {
    Json(body(&state))
}

/// Clear: delete the uploaded book from the server and fall back.
pub async fn delete(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesRootDto>> {
    state.clear().await.map_err(|err| problem(&err))?;
    Ok(Json(body(&state)))
}

/// The one body both verbs answer with — and, with its `uploaded` field
/// filled in, the one `POST /trades/upload` answers with too.
pub(crate) fn body(state: &AppState) -> dto::TradesRootDto {
    // One load, so the folder and the counts below it are the same reload's.
    let published = state.published();
    let (active, book) = (&published.root, &published.book);
    dto::TradesRootDto {
        root: active.dir().map(std::path::Path::to_path_buf),
        source: active.source,
        config_root: state.config.trades.resolved_root(),
        open_trades: book.book.open_trades().count(),
        closed_trades: book.book.closed_trades().count(),
        uploaded_at: active.uploaded_at,
        uploaded: None,
    }
}

/// The statuses `docs/API.md` freezes: the discovery sentence is the 422's
/// `detail`, and a state directory we cannot write (or delete from) is ours
/// to own.
pub(crate) fn problem(err: &SetRootError) -> ApiError {
    let detail = err.to_string();
    match err {
        SetRootError::Discovery(_) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-trades-root",
            detail,
        )
        .titled("Not a trades folder"),
        SetRootError::Persist { .. } | SetRootError::Startup(_) | SetRootError::Interrupted(_) => {
            ApiError::internal(detail)
        }
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

    /// A clear that could not delete the copy must not read as the user's
    /// mistake: they asked for their trades to go, and they have not.
    #[test]
    fn a_state_dir_we_cannot_empty_is_ours_not_the_users_fault() {
        let problem = problem(&SetRootError::Persist {
            path: "/read/only/uploads".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        });
        assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(problem.detail.contains("uploads"), "{problem}");
    }
}
