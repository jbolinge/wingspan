//! Binding and running the HTTP server.
//!
//! The bind policy is checked *before* the listener exists, so a misconfigured
//! remote bind fails with a sentence instead of briefly exposing the book.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config::Config;
use crate::state::AppState;
use crate::watcher;

/// Bind, watch the trades folder, start the refresh loop, and serve until
/// Ctrl-C.
pub async fn run(config: &Config, state: Arc<AppState>) -> anyhow::Result<()> {
    config.check_bind_policy()?;
    let addr = config.server.socket_addr()?;
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!("wingspan {} listening on http://{bound}", crate::VERSION);
    if !crate::statics::embedded_ui() {
        tracing::info!(
            "built without `embed-ui`: run `npm run dev` in web/ and browse http://localhost:5173"
        );
    }
    for note in &state.provider_notes {
        tracing::warn!("{note}");
    }
    serve_on(listener, state).await
}

/// Serve on an already-bound listener (the tests bind port 0).
pub async fn serve_on(listener: TcpListener, state: Arc<AppState>) -> anyhow::Result<()> {
    let _refresh = state.market.spawn();
    let _watch = match watcher::spawn(state.clone(), watcher::DEFAULT_DEBOUNCE) {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!("not watching the trades folder: {err}");
            None
        }
    };
    let app = crate::api::router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Where a test wants the address it actually got.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
