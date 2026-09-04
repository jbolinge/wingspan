//! `GET`/`PUT`/`DELETE /api/v1/trades/root` and `POST /trades/root/probe` —
//! the folder the book is read from, chosen from the status bar instead of
//! from `wingspan.toml`.
//!
//! Trade files live where the user keeps them and are read in place: nothing
//! here copies, moves or deletes a byte of theirs. `GET` says what is loaded
//! and how it got here; `POST …/probe` answers "is this the folder you
//! meant?" without changing anything; `PUT` commits to it; `DELETE` is the
//! Clear button, which stops showing the trades and touches only Wingspan's
//! own state directory.
//!
//! All four share one body (`docs/API.md`), so the SPA renders every one of
//! them with the same code. Everything that decides *which* book wins lives
//! in [`crate::state`], and the bodies and the statuses in [`crate::service`];
//! this module is only the wire.
//!
//! `PUT` and the probe are registered only on a loopback bind — see
//! [`crate::api::router`].

use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use super::json::Json as JsonBody;

use crate::dto;
use crate::error::ApiResult;
use crate::state::AppState;

pub async fn get(State(state): State<Arc<AppState>>) -> Json<dto::TradesRootDto> {
    Json(crate::service::trades_root(&state))
}

/// Pick a folder: validated, persisted, and applied like a restart.
pub async fn put(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradesRootRequest>,
) -> ApiResult<Json<dto::TradesRootDto>> {
    crate::service::set_trades_root(&state, request.root)
        .await
        .map(Json)
}

/// Look, without touching: what a folder holds if it were loaded.
pub async fn probe(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradesRootRequest>,
) -> ApiResult<Json<dto::FolderProbeDto>> {
    crate::service::probe_folder(&state, request.root)
        .await
        .map(Json)
}

/// Clear: stop showing these trades and fall back. Nothing of the user's is
/// read, moved, renamed or removed.
pub async fn delete(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesRootDto>> {
    crate::service::clear_trades_root(&state).await.map(Json)
}

#[cfg(test)]
mod tests {
    use crate::error::ErrorKind;
    use crate::state::SetRootError;
    use wingspan_core::discovery::{DirNames, DiscoveryError};

    #[test]
    fn a_bad_folder_is_a_422_carrying_the_discovery_sentence() {
        let err = SetRootError::Discovery(DiscoveryError::NoTradesDirs {
            start: "/tmp/nope".into(),
            names: DirNames::default(),
        });
        let sentence = err.to_string();
        let problem = crate::service::root_problem(&err);
        assert_eq!(problem.kind, ErrorKind::Unprocessable);
        assert_eq!(problem.detail, sentence);
        assert!(problem.detail.contains("Open Trades"), "{problem}");
    }

    #[test]
    fn a_state_dir_we_cannot_write_is_ours_not_the_users_fault() {
        let problem = crate::service::root_problem(&SetRootError::Persist {
            path: "/read/only/trades_root.json".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        });
        assert_eq!(problem.kind, ErrorKind::Internal);
        assert!(problem.detail.contains("trades_root.json"), "{problem}");
    }
}
