//! Figment-backed configuration. Layered: defaults → `sift.toml` (if
//! present) → `SIFT_` env vars. No file is required for local-mode startup.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentPolicy {
    #[default]
    Personal,
    Team,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    #[default]
    Loopback,
    Network,
    SshProxy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Identity and authorization policy. Independent from how clients reach
    /// the server (ADR-030).
    pub deployment: DeploymentPolicy,
    /// Client-to-server transport topology. `ssh-proxy` is reserved for the
    /// Phase H stdio/proxy transport and is rejected until it is implemented.
    pub transport: Transport,
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
    /// General authenticated API rate limits (Phase F).
    pub rate_limits: RateLimitsConfig,
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
    /// Authoritative externally reachable origin. OAuth callbacks are derived
    /// only from this value, never from request forwarding headers.
    pub public_base_url: Option<String>,
    /// Per-instance GitHub OAuth App client id.
    pub github_client_id: Option<String>,
    /// Per-instance GitHub OAuth App secret. Environment/config only; never
    /// persisted to metadata or included in logs.
    pub github_client_secret: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitsConfig {
    pub trusted_local_exempt: bool,
    pub idle_ttl_secs: u64,
    pub control: Option<RateBucketConfig>,
    pub interactive: Option<RateBucketConfig>,
    pub query: Option<RateBucketConfig>,
    pub heavy_transfer: Option<RateBucketConfig>,
    pub stream_bytes: Option<RateBucketConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateBucketConfig {
    pub refill_per_second: f64,
    pub burst: f64,
    pub cost: f64,
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
            deployment: DeploymentPolicy::default(),
            transport: Transport::default(),
            bind: "127.0.0.1:7474".to_string(),
            log: LogConfig::default(),
            drivers: DriversConfig::default(),
            timeouts: TimeoutConfig::default(),
            auth: AuthConfig::default(),
            metadata: MetadataConfig::default(),
            audit: AuditConfig::default(),
            limits: LimitsConfig::default(),
            rate_limits: RateLimitsConfig::default(),
        }
    }
}

impl Config {
    /// Reject topology/policy combinations that would broaden implicit trust.
    ///
    /// SSH-proxy startup remains deliberately unavailable until its Phase H
    /// transport lands.
    pub fn validate(&self) -> anyhow::Result<()> {
        use anyhow::{bail, Context};

        let bind: std::net::SocketAddr = self
            .bind
            .parse()
            .with_context(|| format!("invalid bind address: {}", self.bind))?;

        if self.transport == Transport::Loopback && !bind.ip().is_loopback() {
            bail!(
                "transport=loopback requires a loopback bind address; got {}",
                self.bind
            );
        }

        if self.auth.loopback_bypass
            && (self.deployment != DeploymentPolicy::Personal
                || self.transport != Transport::Loopback)
        {
            bail!(
                "auth.loopback_bypass is allowed only with deployment=personal and \
                 transport=loopback"
            );
        }

        if self.transport == Transport::SshProxy {
            bail!("transport=ssh-proxy is reserved for Phase H and is not implemented yet");
        }

        let github_partial =
            self.auth.github_client_id.is_some() != self.auth.github_client_secret.is_some();
        if github_partial {
            bail!("GitHub OAuth requires both auth.github_client_id and auth.github_client_secret");
        }
        if self.auth.github_client_id.is_some() && self.auth.public_base_url.is_none() {
            bail!("GitHub OAuth requires auth.public_base_url");
        }
        if let Some(base) = &self.auth.public_base_url {
            let parsed = reqwest::Url::parse(base).context("invalid auth.public_base_url")?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || parsed.username() != ""
                || parsed.password().is_some()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                bail!("auth.public_base_url must be an HTTPS origin without credentials, query, or fragment");
            }
        }

        if self.deployment == DeploymentPolicy::Team {
            if !self.metadata.enabled {
                bail!("deployment=team requires metadata.enabled=true");
            }
            if self.metadata.bootstrap_local {
                bail!("deployment=team requires metadata.bootstrap_local=false");
            }
            if self.metadata.secret_backend == "memory" {
                bail!("deployment=team requires a durable metadata secret backend");
            }
            if self.auth.public_base_url.is_none() {
                bail!("deployment=team requires auth.public_base_url");
            }
        }

        for (name, bucket) in [
            ("control", self.rate_limits.control.as_ref()),
            ("interactive", self.rate_limits.interactive.as_ref()),
            ("query", self.rate_limits.query.as_ref()),
            ("heavy_transfer", self.rate_limits.heavy_transfer.as_ref()),
            ("stream_bytes", self.rate_limits.stream_bytes.as_ref()),
        ] {
            if let Some(bucket) = bucket {
                if !bucket.refill_per_second.is_finite()
                    || bucket.refill_per_second <= 0.0
                    || !bucket.burst.is_finite()
                    || bucket.burst <= 0.0
                    || !bucket.cost.is_finite()
                    || bucket.cost <= 0.0
                    || bucket.cost > bucket.burst
                {
                    bail!("invalid rate_limits.{name}: refill, burst, and cost must be finite and positive, with cost <= burst");
                }
            }
        }

        Ok(())
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

impl Default for RateLimitsConfig {
    fn default() -> Self {
        Self {
            trusted_local_exempt: true,
            idle_ttl_secs: 600,
            control: Some(RateBucketConfig::new(20.0, 40.0, 1.0)),
            interactive: Some(RateBucketConfig::new(30.0, 60.0, 1.0)),
            query: Some(RateBucketConfig::new(10.0, 20.0, 1.0)),
            heavy_transfer: Some(RateBucketConfig::new(2.0, 4.0, 1.0)),
            stream_bytes: Some(RateBucketConfig::new(
                4.0 * 1024.0 * 1024.0,
                8.0 * 1024.0 * 1024.0,
                1.0,
            )),
        }
    }
}

impl Default for RateBucketConfig {
    fn default() -> Self {
        Self::new(1.0, 1.0, 1.0)
    }
}

impl RateBucketConfig {
    const fn new(refill_per_second: f64, burst: f64, cost: f64) -> Self {
        Self {
            refill_per_second,
            burst,
            cost,
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
            public_base_url: None,
            github_client_id: None,
            github_client_secret: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_personal_loopback() {
        let config = Config::default();
        assert_eq!(config.deployment, DeploymentPolicy::Personal);
        assert_eq!(config.transport, Transport::Loopback);
        config.validate().unwrap();
    }

    #[test]
    fn loopback_transport_rejects_network_bind() {
        let config = Config {
            bind: "0.0.0.0:7474".into(),
            ..Config::default()
        };
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("transport=loopback"));
    }

    #[test]
    fn network_transport_rejects_loopback_bypass() {
        let config = Config {
            transport: Transport::Network,
            ..Config::default()
        };
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("loopback_bypass"));
    }

    #[test]
    fn team_requires_durable_metadata_and_ssh_is_unavailable() {
        let team = Config {
            deployment: DeploymentPolicy::Team,
            auth: AuthConfig {
                loopback_bypass: false,
                public_base_url: Some("https://sift.example.test".into()),
                ..AuthConfig::default()
            },
            metadata: MetadataConfig {
                bootstrap_local: false,
                secret_backend: "file".into(),
                ..MetadataConfig::default()
            },
            ..Config::default()
        };
        team.validate().unwrap();

        let unsafe_team = Config {
            deployment: DeploymentPolicy::Team,
            auth: AuthConfig {
                loopback_bypass: false,
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        assert!(unsafe_team
            .validate()
            .unwrap_err()
            .to_string()
            .contains("bootstrap_local"));

        let ssh = Config {
            transport: Transport::SshProxy,
            auth: AuthConfig {
                loopback_bypass: false,
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        assert!(ssh
            .validate()
            .unwrap_err()
            .to_string()
            .contains("not implemented"));
    }

    #[test]
    fn github_oauth_configuration_is_complete_and_uses_an_https_origin() {
        let missing_secret = Config {
            auth: AuthConfig {
                github_client_id: Some("client".into()),
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        assert!(missing_secret.validate().is_err());

        let insecure = Config {
            auth: AuthConfig {
                public_base_url: Some("http://sift.example.test".into()),
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        assert!(insecure.validate().is_err());

        let configured = Config {
            auth: AuthConfig {
                public_base_url: Some("https://sift.example.test".into()),
                github_client_id: Some("client".into()),
                github_client_secret: Some("secret".into()),
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        configured.validate().unwrap();
    }

    #[test]
    fn rate_limit_configuration_rejects_invalid_buckets() {
        let mut config = Config::default();
        config.rate_limits.query = Some(RateBucketConfig {
            refill_per_second: 1.0,
            burst: 1.0,
            cost: 2.0,
        });
        assert!(config.validate().unwrap_err().to_string().contains("query"));
    }
}
