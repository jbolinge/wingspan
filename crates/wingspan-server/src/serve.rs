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
    tracing::info!("gate: {}", config.gate().label());
    if config.server.lan_only && !config.server.trusted_proxies.is_empty() {
        tracing::info!(
            "trusting X-Forwarded-For from {}",
            config.server.trusted_proxies.join(", ")
        );
    }
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
    // Files each published index level under its own session, so the statement
    // rung has that day's close to calibrate against tomorrow.
    let _closes = state.spawn_close_recorder();
    // The state owns the watch so that adopting an upload (or clearing one)
    // can move it to whatever book is in force — including nowhere.
    if let Err(err) = state.watch_trades(watcher::DEFAULT_DEBOUNCE) {
        tracing::warn!("not watching the trades folder: {err}");
    }
    let app = crate::api::router(state);
    // `into_make_service_with_connect_info` is what puts the TCP peer address
    // in the request extensions; the `lan_only` gate has nothing to judge
    // without it, and fails closed rather than guessing.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
