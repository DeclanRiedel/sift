//! Figment-backed configuration. Layered: defaults → `sift.toml` (if
//! present) → `SIFT_` env vars. No file is required for local-mode startup.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Socket address to bind the HTTP server on.
    pub bind: String,
    /// RUST_LOG-style filter (`sift=debug,info`).
    pub log: LogConfig,
    /// Driver-registration knobs.
    pub drivers: DriversConfig,
    /// Operational timeouts.
    pub timeouts: TimeoutConfig,
    /// Minimal Phase 0 auth hook.
    pub auth: AuthConfig,
    /// Local metadata store configuration.
    pub metadata: MetadataConfig,
    /// Audit/replay log configuration.
    pub audit: AuditConfig,
    /// Result-size limits for synchronous responses.
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing-subscriber` env-filter directive string.
    pub filter: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DriversConfig {
    /// If true, register `MockDriver` for engine `postgres` (overriding the
    /// real `PgDriver`). Useful for headless tests and demos without a DB.
    pub mock: bool,
    /// If true, register `MockDriver` for an extra synthetic engine slot.
    /// Off by default.
    pub mock_extra: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeoutConfig {
    /// Per-request timeout for synchronous ops (ping/schema/execute HTTP).
    pub request_secs: u64,
    /// Deadline for draining in-flight queries during graceful shutdown
    /// (ADR-018). `0` waits indefinitely for queries to finish.
    pub shutdown_drain_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// If set, non-loopback clients must send `Authorization: Bearer <token>`.
    /// Empty by default for local-first development.
    pub bearer_token: Option<String>,
    /// Zero-auth local mode. The current implementation applies this for the
    /// local server process; peer-address scoping lands with hosted mode.
    pub loopback_bypass: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataConfig {
    /// Enable the local metadata store.
    pub enabled: bool,
    /// Optional SQLite path. Defaults to the platform-local state path.
    pub path: Option<String>,
    /// Secret backend: `memory` | `file` | `keychain`. `keychain` requires the
    /// server to be built with the `os-keychain` feature.
    pub secret_backend: String,
    /// Path to the 32-byte key file for the `file` secret backend. Required
    /// when `secret_backend = "file"`. Set via `SIFT_METADATA__SECRET_KEY_FILE`
    /// (the nix dev shell exports it).
    pub secret_key_file: Option<String>,
    /// Bootstrap implicit local tenant/principal when the DB is empty.
    pub bootstrap_local: bool,
    /// Persist raw SQL text in query history. When false, only a normalized
    /// fingerprint is stored (the audit/replay trail is always fingerprinted).
    pub store_sql: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Max rows a synchronous HTTP execute may return before `ResultTooLarge`.
    pub max_http_result_rows: usize,
    /// Max approximate bytes a synchronous HTTP execute may return before
    /// `ResultTooLarge`. Guards against a few very wide rows OOMing the server.
    pub max_http_result_bytes: usize,
    /// Max simultaneously-open cursors per session (ADR-011). Opening a
    /// new cursor when at cap evicts the session's LRA cursor.
    pub max_cursors_per_session: usize,
    /// Pages the cursor pump buffers ahead of the consumer (ADR-011).
    /// Also sets automatic backpressure — a slow consumer stalls the
    /// pump at this depth.
    pub cursor_prefetch_pages: usize,
    /// Directory for on-eviction cursor spill files (ADR-011). Empty
    /// disables spill.
    pub cursor_spill_dir: Option<String>,
    /// Time-to-live in seconds for spill files. Reaped after this if
    /// the client never resumes. Default 600 (10 min).
    pub cursor_spill_ttl_secs: u64,
    /// Schema cache TTL in seconds. Cached SchemaSnapshot entries expire
    /// after this even if invalidation is missed. Default 60.
    pub schema_cache_ttl_secs: u64,
    /// Poll interval in seconds for the SQL Server schema invalidator
    /// (`sys.objects.modify_date`). Default 30.
    pub schema_mssql_poll_secs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuditConfig {
    /// Optional JSONL path for replayable operation audit rows.
    pub operation_log_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7474".to_string(),
            log: LogConfig::default(),
            drivers: DriversConfig::default(),
            timeouts: TimeoutConfig::default(),
            auth: AuthConfig::default(),
            metadata: MetadataConfig::default(),
            audit: AuditConfig::default(),
            limits: LimitsConfig::default(),
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_http_result_rows: 10_000,
            max_http_result_bytes: 16 * 1024 * 1024,
            max_cursors_per_session: 32,
            cursor_prefetch_pages: 2,
            cursor_spill_dir: None,
            cursor_spill_ttl_secs: 600,
            schema_cache_ttl_secs: 60,
            schema_mssql_poll_secs: 30,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: "sift=info,tower_http=info".to_string(),
        }
    }
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            request_secs: 30,
            shutdown_drain_secs: 30,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            bearer_token: None,
            loopback_bypass: true,
        }
    }
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            secret_backend: "memory".to_string(),
            secret_key_file: None,
            bootstrap_local: true,
            store_sql: true,
        }
    }
}

/// Load config from `sift.toml` (if present) then `SIFT_*` env vars, falling
/// back to defaults. Missing file is not an error.
pub fn load() -> anyhow::Result<Config> {
    use figment::providers::{Env, Format, Toml};
    let fig = figment::Figment::new()
        .merge(figment::providers::Serialized::defaults(Config::default()))
        .merge(Toml::file("sift.toml"))
        .merge(Env::prefixed("SIFT_").split("__"));
    Ok(fig.extract()?)
}
