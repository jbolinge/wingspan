//! `wingspan-devserver`: the dev-only HTTP front end.
//!
//! The shipped product is the Tauri desktop app; this crate exists so the SPA
//! can be developed in a browser (`npm run dev` against `--dev`) and so
//! `web/e2e` has something to drive. It is a *transport*: the router, the SSE
//! plumbing, the problem+json adapter and the embedded bundle, all wrapping
//! [`wingspan_app::service`] and adding no behaviour of its own.
//!
//! It is never bundled and never installed. The listener is 127.0.0.1 and
//! nothing else — there is no bind address to configure and no deployment to
//! be.

pub mod api;
pub mod serve;
pub mod sse;
pub mod statics;
pub mod testing;
