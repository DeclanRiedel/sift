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
    /// Secret backend. Only `memory` exists today; keyring/file land later.
    pub secret_backend: String,
    /// Bootstrap implicit local tenant/principal when the DB is empty.
    pub bootstrap_local: bool,
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
        Self { request_secs: 30 }
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
            bootstrap_local: true,
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
