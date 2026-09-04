//! Shared test scaffolding: a pinned app over `fixtures/trades`.
//!
//! Public so the integration tests in `tests/` use exactly the wiring the
//! binary does — a test that builds its own router proves nothing about the
//! one that ships.
//!
//! "Demo" here means the demo *wiring* — synthetic prices, the pinned clock,
//! `RootSource::Demo` — applied to the anonymised test fixtures, whose golden
//! numbers the tests check. The book `wingspan serve --demo` actually shows is
//! the made-up one at [`demo_book_root`].

use std::sync::Arc;

use crate::state::AppState;

// The fixtures, the pinned clock and the demo wiring live in [`crate::demo`],
// which has no test harness in it; re-exported here so every existing
// `testing::` call site keeps working.
pub use crate::demo::{
    clock, clock_at, demo_book_root, demo_config, demo_options, demo_state, fixtures_root,
    scratch_dir, valuation_date,
};

/// A running server on an ephemeral port, stopped when dropped.
#[derive(Debug)]
pub struct ServerHandle {
    pub addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ServerHandle {
    /// Absolute URL for an API path (`"/trades"` -> `http://…/api/v1/trades`).
    #[must_use]
    pub fn api(&self, path: &str) -> String {
        format!("http://{}/api/v1{path}", self.addr)
    }

    /// Absolute URL for anything else.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

/// Bind `127.0.0.1:0` and serve `state` there.
///
/// # Panics
/// If the ephemeral port cannot be bound.
pub async fn spawn_server(state: Arc<AppState>) -> ServerHandle {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let addr = listener.local_addr().expect("a local address");
    let task = tokio::spawn(async move {
        if let Err(err) = crate::serve::serve_on(listener, state).await {
            eprintln!("test server stopped: {err}");
        }
    });
    ServerHandle { addr, task }
}

/// The same server, but every connection is reported as coming from `peer`.
///
/// A test cannot open a socket from `203.0.113.5`, and the `lan_only` gate is
/// entirely about which address the socket came from — so the peer is
/// overwritten in the extensions, outside the router and therefore outside the
/// gate itself. Everything below (the gate, `X-Forwarded-For` resolution, the
/// host guard, the handlers) is the wiring the binary runs.
///
/// # Panics
/// If the ephemeral port cannot be bound.
pub async fn spawn_server_from(state: Arc<AppState>, peer: std::net::SocketAddr) -> ServerHandle {
    use axum::extract::ConnectInfo;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let addr = listener.local_addr().expect("a local address");
    let app = crate::api::router(state).layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| async move {
            request.extensions_mut().insert(ConnectInfo(peer));
            next.run(request).await
        },
    ));
    let task = tokio::spawn(async move {
        let served = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
        if let Err(err) = served {
            eprintln!("test server stopped: {err}");
        }
    });
    ServerHandle { addr, task }
}

/// A demo app already serving on an ephemeral port.
pub async fn demo_server() -> ServerHandle {
    spawn_server(demo_state().await).await
}
