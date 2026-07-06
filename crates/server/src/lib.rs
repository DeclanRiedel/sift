#![recursion_limit = "256"]

//! `sift-server` — workspace server. The server is the product; clients
//! (desktop GPUI, future web) are thin stateless consumers of its HTTP API
//! (ADR-001, ADR-002). Local-first by default (ADR-010): same binary runs
//! in-process alongside the desktop client or as a daemon for hosted use.
//!
//! Phase 0 step 5/6 surface: axum bootstrap, figment config, tracing,
//! driver registry, session + connection manager, and a synchronous HTTP
//! surface for the Tier 0 operations. WebSocket streaming (step 10),
//! auth (step 11), OpenAPI (step 12), and client-sdk (step 13) come next.

pub mod config;
pub mod error;
pub mod http;
pub mod registry;
pub mod room_runtime;
pub mod session;

pub use config::Config;
pub use error::ApiError;
pub use registry::DriverRegistry;
pub use room_runtime::RoomRuntime;
pub use session::{ConnectionEntry, Session, SessionStore};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
