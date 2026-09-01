//! Connection specifications + post-connect server-reported metadata.

use crate::ProviderRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// Plaintext for now; later moved to OS keychain. The field stays —
    /// the *source* changes.
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
    /// Deadpool max_size override. Defaults to 8.
    pub pool_max_size: Option<u32>,
    /// Number of connections to pre-warm at `open` time so the first
    /// query does not pay the connect handshake. Best-effort; `open`
    /// still succeeds if pre-warm fails (e.g. temporary DB pressure).
    pub pool_min_size: Option<u32>,
    /// PostgreSQL settings applied with `set_config` whenever Sift opens
    /// a logical connection from the pool.
    #[serde(default)]
    pub session_variables: BTreeMap<String, String>,
    /// SQL batches run after session variables are applied.
    #[serde(default)]
    pub startup_sql: Vec<String>,
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
    /// Number of warm idle SQL Server connections to keep ready in
    /// the driver's per-spec pool. `open()` first tries the warm pool
    /// before opening a fresh TDS session; a background top-up
    /// refills after each pop. Best-effort — a failing top-up logs
    /// at debug and leaves the pool cold.
    pub pool_min_size: Option<u32>,
    /// Values applied through `sp_set_session_context` on every fresh TDS
    /// session, including sessions created by the warm pool.
    #[serde(default)]
    pub session_variables: BTreeMap<String, String>,
    /// SQL batches run after session variables are applied.
    #[serde(default)]
    pub startup_sql: Vec<String>,
}

/// Reported by `Driver::ping` after a successful round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerInfo {
    pub provider: ProviderRef,
    pub server_version: String,
    pub current_database: String,
    pub current_user: String,
    /// Number of pre-warmed idle connections currently sitting in the
    /// driver's per-spec pool for the spec this handle came from.
    /// `Some(0)` means the pool is cold; `Some(n)` means the next
    /// `open()` for the same spec will be served from a warm slot.
    /// `None` when the driver doesn't track warmth (older drivers, or
    /// a driver whose pool concept doesn't apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_warm_slots: Option<u32>,
}

/// Connection access mode at open time (read-only vs read-write). Distinct
/// from transaction access mode (which can be stricter per-tx).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_options_are_backward_compatible() {
        let config: EngineConnectionSpec = serde_json::from_value(serde_json::json!({
            "engine": "postgres",
            "application_name": "sift"
        }))
        .unwrap();
        let EngineConnectionSpec::Postgres(config) = config else {
            panic!("expected postgres configuration")
        };
        assert!(config.session_variables.is_empty());
        assert!(config.startup_sql.is_empty());
    }

    #[test]
    fn initialization_options_round_trip() {
        let config = EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
            session_variables: BTreeMap::from([("tenant".into(), "analytics".into())]),
            startup_sql: vec!["SET DEADLOCK_PRIORITY LOW".into()],
            ..Default::default()
        });
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["session_variables"]["tenant"], "analytics");
        assert_eq!(value["startup_sql"][0], "SET DEADLOCK_PRIORITY LOW");
    }
}
