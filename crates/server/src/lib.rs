#![recursion_limit = "512"]

//! `sift-server` — workspace server. The server is the product; clients
//! (desktop GPUI, future web) are thin stateless consumers of its HTTP API
//! (ADR-001, ADR-002). Local-first by default (ADR-010): same binary runs
//! in-process alongside the desktop client or as a daemon for hosted use.

pub mod authorization;
pub mod autocomplete;
pub mod automation;
pub mod capability;
pub mod comparison;
pub mod config;
pub mod connection_pipeline;
pub mod correlation;
pub mod csv_import;
pub mod cursors;
pub mod ddl;
pub mod ddl_source;
pub mod document_actor;
pub mod document_registry;
pub mod edit;
pub mod error;
pub mod export;
pub mod extension_dispatch;
pub mod extension_runtime;
pub mod fingerprint;
pub mod git_adapter;
pub mod http;
pub mod identity;
pub mod metadata_runtime;
pub mod migration;
pub mod plan;
pub mod process;
pub mod rate_limit;
pub mod registry;
pub mod remote_agent;
pub mod resources;
pub mod room_results;
pub mod room_runtime;
pub mod room_service;
mod rpc_provider;
pub mod run_executor;
pub mod runtime;
pub mod schema_cache;
pub mod search;
pub mod session;
pub mod shutdown;
pub mod sql_policy;
pub mod state_backup;
pub mod updater;
pub mod workspace_adapter;
pub mod workspace_projection;

pub use config::Config;
pub use error::ApiError;
pub use registry::{
    BuiltinProviderAdapter, DatabaseProvider, DriverRegistry, ProviderRegistry, ProviderServerInfo,
    RegisteredProvider,
};
pub use room_runtime::RoomRuntime;
pub use rpc_provider::RpcProvider;
pub use session::{ConnectionEntry, ConnectionProvenance, Session, SessionStore};
pub use shutdown::Shutdown;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
