#![recursion_limit = "256"]

//! `sift-server` — workspace server. The server is the product; clients
//! (desktop GPUI, future web) are thin stateless consumers of its HTTP API
//! (ADR-001, ADR-002). Local-first by default (ADR-010): same binary runs
//! in-process alongside the desktop client or as a daemon for hosted use.

pub mod autocomplete;
pub mod config;
pub mod correlation;
pub mod cursors;
pub mod ddl;
pub mod edit;
pub mod error;
pub mod export;
pub mod fingerprint;
pub mod http;
pub mod plan;
pub mod process;
pub mod registry;
pub mod room_runtime;
pub mod schema_cache;
pub mod search;
pub mod session;
pub mod shutdown;

pub use config::Config;
pub use error::ApiError;
pub use registry::DriverRegistry;
pub use room_runtime::RoomRuntime;
pub use session::{ConnectionEntry, Session, SessionStore};
pub use shutdown::Shutdown;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
