//! Binding and running the HTTP server.
//!
//! The listener is 127.0.0.1 and nothing else: this is a local dev server for
//! the SPA, not a deployment, so there is no bind address to configure and no
//! policy to check before the socket exists.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use wingspan_app::serve::spawn_background;
use wingspan_app::state::AppState;

/// Bind loopback on `port`, watch the trades folder, start the refresh loop,
/// and serve.
pub async fn run(port: u16, state: Arc<AppState>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let bound = listener.local_addr()?;
    tracing::info!(
        "wingspan-devserver {} listening on http://{bound}",
        wingspan_app::VERSION
    );
    if !crate::statics::embedded_ui() {
        tracing::info!(
            "built without `embed-ui`: run `npm run dev` in web/ and browse http://localhost:5173"
        );
    }
    for note in state.notes() {
        tracing::warn!("{note}");
    }
    serve_on(listener, state).await
}

/// Serve on an already-bound listener (the tests bind port 0).
pub async fn serve_on(listener: TcpListener, state: Arc<AppState>) -> anyhow::Result<()> {
    // Held for as long as we serve: dropping it aborts the refresh loop, the
    // close recorder and the trades watcher.
    let _background = spawn_background(&state);
    let app = crate::api::router(state);
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

/// Where a test wants the address it actually got.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}
