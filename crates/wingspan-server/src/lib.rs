//! `wingspan-server`: the `wingspan` binary's guts — configuration, the axum
//! API, SSE, the trades-folder watcher, the embedded SPA and the CLI.
//!
//! The crate is a library so the integration tests can drive exactly the
//! router and state the binary runs. Nothing here re-implements domain logic:
//! `wingspan-core` assembles the book, `wingspan-pricing` prices it,
//! `wingspan-marketdata` fetches the quotes, and this crate is the wiring plus
//! the wire format frozen in `docs/API.md`.
//!
//! ```no_run
//! # async fn go() -> anyhow::Result<()> {
//! use wingspan_server::{config, serve, state::{AppOptions, AppState}};
//!
//! let loaded = config::load(None)?;
//! let state = AppState::build(loaded.config.clone(), AppOptions::default()).await?;
//! serve::run(&loaded.config, state).await?;
//! # Ok(()) }
//! ```

pub mod api;
pub mod cli;
pub mod closes;
pub mod config;
pub mod dto;
pub mod env_file;
pub mod error;
pub mod providers;
pub mod root_override;
pub mod serve;
pub mod sse;
pub mod state;
pub mod statics;
pub mod testing;
pub mod uploads;
pub mod watcher;

/// The version reported by `/api/v1/health` and `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
