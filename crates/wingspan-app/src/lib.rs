//! `wingspan-app`: the application, with no transport in it — configuration,
//! the trades book and the market behind it, the service layer every front
//! end wraps, the change events, the folder watcher and the diagnostics.
//!
//! Three front ends link this crate and add a transport of their own: the
//! shipped Tauri desktop app (`web/src-tauri`, which turns each
//! [`service`] function into a `#[tauri::command]`), the dev-only HTTP server
//! (`crates/wingspan-devserver`) the SPA is developed against, and the
//! headless CLI (`crates/wingspan-cli`). Nothing here links axum, clap or
//! tauri, so the same answers reach all three by construction.
//!
//! Nothing here re-implements domain logic either: `wingspan-core` assembles
//! the book, `wingspan-pricing` prices it, `wingspan-marketdata` fetches the
//! quotes, and this crate is the wiring plus the response bodies frozen in
//! `docs/IPC.md`.
//!
//! The crate also owns the trade-source model ([`source`]): a Google Sheet
//! (fetched over HTTPS), a local CSV file, or the transitional folder
//! scanner, each producing a [`wingspan_core::book::Book`] the same way.
//! [`saved_source`] remembers which source was chosen and its recent
//! history, the way [`saved_root`] used to remember a chosen folder.
//!
//! ```no_run
//! # async fn go() -> anyhow::Result<()> {
//! use wingspan_app::{config, service};
//! use wingspan_app::state::{AppOptions, AppState};
//!
//! let loaded = config::load(None, config::Layers::all())?;
//! let state = AppState::build(loaded.config, AppOptions::default()).await?;
//! let trades = service::trades(&state)?;
//! println!("{} open trades", trades.trades.len());
//! # Ok(()) }
//! ```

pub mod closes;
pub mod config;
pub mod demo;
pub mod diagnostics;
pub mod dto;
pub mod env_file;
pub mod error;
pub mod events;
pub mod paths;
pub mod providers;
pub mod saved_root;
pub mod saved_source;
pub mod serve;
pub mod service;
pub mod settings;
pub mod sheet_url;
pub mod source;
pub mod state;
pub mod watcher;

/// The version reported by `/api/v1/health` and `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
