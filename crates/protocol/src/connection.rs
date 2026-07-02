//! Connection specifications + post-connect server-reported metadata.

use crate::Engine;
use serde::{Deserialize, Serialize};

/// All a driver needs to open a connection. The engine is NOT carried here
/// — the caller (server registry, MockDriver tests) already knows which
/// engine the spec is destined for, because drivers are registered per
/// engine. Carrying `engine` here collided with `OpenConnectionRequest`'s
/// `#[serde(flatten)]` of the spec; the envelope is the single source of
/// truth for engine selection.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConnectionSpec {
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: String,
    /// Plaintext for now; Phase 0 step 22 (BACKEND.md) moves secrets to OS
    /// keychain. The field stays — the *source* changes.
    pub password: Option<String>,
    pub ssl_mode: Option<SslMode>,
    pub engine_specific: Option<EngineConnectionSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum EngineConnectionSpec {
    Postgres(PgConnectionSpec),
    SqlServer(MssqlConnectionSpec),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PgConnectionSpec {
    /// PostgreSQL `search_path` to set on connect.
    pub search_path: Option<Vec<String>>,
    /// `application_name` for `pg_stat_activity` visibility.
    pub application_name: Option<String>,
    /// Connect timeout per attempt.
    pub connect_timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MssqlConnectionSpec {
    /// Enable Multiple Active Result Sets on the connection.
    pub mars: bool,
    /// SQL Server `Encrypt` option (TDS encryption toggle).
    pub encrypt: Option<bool>,
    /// `TrustServerCertificate`.
    pub trust_server_certificate: Option<bool>,
    /// Connect timeout per attempt.
    pub connect_timeout_secs: Option<u32>,
}

/// Reported by `Driver::ping` after a successful round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerInfo {
    pub engine: Engine,
    pub server_version: String,
    pub current_database: String,
    pub current_user: String,
}

/// Connection access mode at open time (read-only vs read-write). Distinct
/// from transaction access mode (which can be stricter per-tx).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}
