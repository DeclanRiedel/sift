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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    /// Parent-owned foreground lifecycle. A future desktop may link the
    /// runtime directly; the standalone binary remains foreground-owned.
    #[default]
    InProcess,
    /// Long-lived user/service process with singleton descriptor state.
    Daemon,
    /// Immutable image managed and updated by a container orchestrator.
    Container,
}

impl std::str::FromStr for RuntimeMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "in-process" => Ok(Self::InProcess),
            "daemon" => Ok(Self::Daemon),
            "container" => Ok(Self::Container),
            _ => Err(format!(
                "invalid runtime mode `{value}`; expected in-process, daemon, or container"
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Identity and authorization policy. Independent from how clients reach
    /// the server (ADR-030).
    pub deployment: DeploymentPolicy,
    /// Client-to-server transport topology. `ssh-proxy` is a daemon-only,
    /// explicitly authenticated remote-loopback transport.
    pub transport: Transport,
    /// Process ownership and activation policy, independent from transport.
    pub mode: RuntimeMode,
    /// Runtime identity/descriptor storage.
    pub runtime: RuntimeConfig,
    /// Signed release checks and immutable staging (ADR-015).
    pub updater: UpdaterConfig,
    /// Socket address to bind the HTTP server on.
    pub bind: String,
    /// RUST_LOG-style filter (`sift=debug,info`).
    pub log: LogConfig,
    /// Driver-registration knobs.
    pub drivers: DriversConfig,
    /// Operator-controlled extension development policy.
    pub extensions: ExtensionsConfig,
    /// Operational timeouts.
    pub timeouts: TimeoutConfig,
    /// Minimal auth hook.
    pub auth: AuthConfig,
    /// Local metadata store configuration.
    pub metadata: MetadataConfig,
    /// Audit/replay log configuration.
    pub audit: AuditConfig,
    /// Result-size limits for synchronous responses.
    pub limits: LimitsConfig,
    /// General authenticated API rate limits.
    pub rate_limits: RateLimitsConfig,
    /// Default and operator-maximum per-tenant resource limits.
    pub tenant_limits: TenantLimitsConfig,
    /// Optional server-side workspace filesystem projections. Virtual
    /// workspaces remain available when this is disabled or empty.
    pub workspaces: WorkspaceProjectionConfig,
    /// Bundled Git adapter. Disabled unless both this and workspace projections
    /// are explicitly enabled by the operator.
    pub vcs: VcsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WorkspaceProjectionConfig {
    pub enabled: bool,
    /// Operator-owned roots addressed by opaque handles in public APIs.
    pub roots: Vec<WorkspaceRootConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRootConfig {
    pub handle: String,
    pub path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VcsConfig {
    pub enabled: bool,
    pub network_enabled: bool,
    /// Absolute fixed Git executable. If absent, the enabled adapter resolves
    /// `git` once at startup and records the canonical executable observation.
    pub executable: Option<String>,
    pub local_timeout_secs: u64,
    pub network_timeout_secs: u64,
    pub max_output_bytes: usize,
    pub max_file_bytes: usize,
    pub max_status_entries: usize,
    pub max_history_page: u32,
    pub max_commit_files: usize,
    pub max_diff_files: usize,
    pub max_diff_hunks: usize,
    pub max_diff_lines: usize,
}

impl Default for VcsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            network_enabled: false,
            executable: None,
            local_timeout_secs: 30,
            network_timeout_secs: 120,
            max_output_bytes: 8 * 1024 * 1024,
            max_file_bytes: 8 * 1024 * 1024,
            max_status_entries: 20_000,
            max_history_page: 200,
            max_commit_files: 5_000,
            max_diff_files: 2_000,
            max_diff_hunks: 4_000,
            max_diff_lines: 200_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing-subscriber` env-filter directive string.
    pub filter: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Directory for the stable instance id, daemon lock, and ready
    /// descriptor. Defaults beside the metadata database.
    pub state_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdaterConfig {
    /// Check and stage signed releases in the background.
    pub enabled: bool,
    /// Explicit signed release channel.
    pub channel: String,
    /// Distribution-owned manifest URL. Never inferred from another host.
    pub manifest_url: Option<String>,
    /// Detached signature URL for the exact manifest bytes.
    pub signature_url: Option<String>,
    /// Private updater state/cache root. Defaults under runtime state.
    pub state_dir: Option<String>,
    /// Hard download ceiling, in addition to the signed artifact length.
    pub max_artifact_bytes: u64,
    /// Delay before the first background check.
    pub initial_delay_secs: u64,
    /// Base interval between successful or failed background checks.
    pub check_interval_secs: u64,
    /// Uniform random delay added to each background interval.
    pub jitter_secs: u64,
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: "stable".into(),
            manifest_url: None,
            signature_url: None,
            state_dir: None,
            max_artifact_bytes: 512 * 1024 * 1024,
            initial_delay_secs: 30,
            check_interval_secs: 6 * 60 * 60,
            jitter_secs: 10 * 60,
        }
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionsConfig {
    /// Canonicalized local development directories registered at startup.
    pub development_overrides: Vec<String>,
    /// Team deployments reject development paths unless explicitly enabled.
    pub allow_hosted_development: bool,
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
    /// Maximum normalized plan-tree plus warning bytes per durable capture.
    /// May only lower the built-in 8 MiB ceiling.
    pub plan_capture_max_bytes: usize,
    /// Durable plan captures retained per tenant. May only lower 5000.
    pub plan_capture_max_per_tenant: i64,
    /// Durable plan captures retained per semantic source. May only lower 50.
    pub plan_capture_max_per_source: i64,
    /// Maximum durable plan-capture age in days. May only lower 30.
    pub plan_capture_max_age_days: i64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TenantLimitsConfig {
    pub trusted_local_unlimited: bool,
    pub defaults: sift_protocol::TenantResourceLimits,
    pub ceilings: sift_protocol::TenantResourceLimits,
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
            mode: RuntimeMode::default(),
            runtime: RuntimeConfig::default(),
            updater: UpdaterConfig::default(),
            bind: "127.0.0.1:7474".to_string(),
            log: LogConfig::default(),
            drivers: DriversConfig::default(),
            extensions: ExtensionsConfig::default(),
            timeouts: TimeoutConfig::default(),
            auth: AuthConfig::default(),
            metadata: MetadataConfig::default(),
            audit: AuditConfig::default(),
            limits: LimitsConfig::default(),
            rate_limits: RateLimitsConfig::default(),
            tenant_limits: TenantLimitsConfig::default(),
            workspaces: WorkspaceProjectionConfig::default(),
            vcs: VcsConfig::default(),
        }
    }
}

impl Config {
    /// Reject topology/policy combinations that would broaden implicit trust.
    ///
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

        if self.transport == Transport::SshProxy && !bind.ip().is_loopback() {
            bail!(
                "transport=ssh-proxy requires a remote loopback bind address; got {}",
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

        if self.transport == Transport::SshProxy && self.mode != RuntimeMode::Daemon {
            bail!("transport=ssh-proxy requires mode=daemon");
        }
        if self.mode == RuntimeMode::Container && self.transport == Transport::SshProxy {
            bail!("mode=container cannot use transport=ssh-proxy");
        }
        if self.mode == RuntimeMode::Container && self.updater.enabled {
            bail!("mode=container disables self-update; replace the container image instead");
        }
        if self.updater.enabled {
            if self.updater.channel.is_empty()
                || !self
                    .updater
                    .channel
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                bail!("updater.channel must be a non-empty safe channel name");
            }
            if self.updater.max_artifact_bytes == 0 {
                bail!("updater.max_artifact_bytes must be greater than zero");
            }
            if self.updater.check_interval_secs == 0 {
                bail!("updater.check_interval_secs must be greater than zero");
            }
            if self.updater.jitter_secs > 24 * 60 * 60 {
                bail!("updater.jitter_secs must not exceed 24 hours");
            }
            for (name, value) in [
                ("updater.manifest_url", &self.updater.manifest_url),
                ("updater.signature_url", &self.updater.signature_url),
            ] {
                let value = value
                    .as_deref()
                    .with_context(|| format!("{name} is required when updater.enabled=true"))?;
                let url = reqwest::Url::parse(value)
                    .with_context(|| format!("{name} is not a valid URL"))?;
                if url.scheme() != "https" {
                    bail!("{name} must use HTTPS");
                }
            }
        }
        if self.transport == Transport::SshProxy
            && (!self.metadata.enabled || self.metadata.secret_backend == "memory")
        {
            bail!(
                "transport=ssh-proxy requires metadata.enabled=true and a durable secret backend"
            );
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

        if self.workspaces.enabled && self.workspaces.roots.is_empty() {
            bail!("workspaces.enabled=true requires at least one configured root");
        }
        if self.vcs.enabled && !self.workspaces.enabled {
            bail!("vcs.enabled=true requires workspaces.enabled=true");
        }
        if self.vcs.network_enabled && !self.vcs.enabled {
            bail!("vcs.network_enabled=true requires vcs.enabled=true");
        }
        if !(1..=300).contains(&self.vcs.local_timeout_secs)
            || !(1..=900).contains(&self.vcs.network_timeout_secs)
        {
            bail!("VCS timeouts must be positive and within their built-in ceilings");
        }
        if !(1..=64 * 1024 * 1024).contains(&self.vcs.max_output_bytes)
            || !(1..=64 * 1024 * 1024).contains(&self.vcs.max_file_bytes)
            || !(1..=100_000).contains(&self.vcs.max_status_entries)
            || !(1..=1_000).contains(&self.vcs.max_history_page)
            || !(1..=25_000).contains(&self.vcs.max_commit_files)
            || !(1..=10_000).contains(&self.vcs.max_diff_files)
            || !(1..=20_000).contains(&self.vcs.max_diff_hunks)
            || !(1..=1_000_000).contains(&self.vcs.max_diff_lines)
        {
            bail!("VCS output, file, status, history, commit, and diff limits must be positive and within their built-in ceilings");
        }
        if let Some(executable) = &self.vcs.executable {
            let path = std::path::Path::new(executable);
            if !path.is_absolute() {
                bail!("vcs.executable must be an absolute path");
            }
            let metadata =
                std::fs::symlink_metadata(path).with_context(|| "vcs.executable is unavailable")?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("vcs.executable must be a real file, not a symlink");
            }
        }
        let mut root_handles = std::collections::HashSet::new();
        let mut canonical_roots = std::collections::HashSet::new();
        for root in self
            .workspaces
            .roots
            .iter()
            .filter(|_| self.workspaces.enabled)
        {
            if root.handle.is_empty()
                || root.handle.len() > 64
                || !root
                    .handle
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                || !root_handles.insert(root.handle.clone())
            {
                bail!("workspace root handles must be unique safe names of at most 64 bytes");
            }
            let path = std::path::Path::new(&root.path);
            if !path.is_absolute() {
                bail!("workspace root `{}` must be an absolute path", root.handle);
            }
            let metadata = std::fs::symlink_metadata(path)
                .with_context(|| format!("workspace root `{}` is unavailable", root.handle))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "workspace root `{}` must be a real directory, not a symlink",
                    root.handle
                );
            }
            let canonical = std::fs::canonicalize(path)
                .with_context(|| format!("workspace root `{}` is unavailable", root.handle))?;
            if !canonical_roots.insert(canonical) {
                bail!("workspace roots must not alias the same directory");
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

    pub fn runtime_state_dir(&self) -> std::path::PathBuf {
        if let Some(path) = self
            .runtime
            .state_dir
            .as_deref()
            .filter(|path| !path.is_empty())
        {
            return path.into();
        }
        let metadata = self
            .metadata
            .path
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(sift_metadata::MetadataStore::default_local_path);
        metadata
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| ".".into())
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
            plan_capture_max_bytes: 8 * 1024 * 1024,
            plan_capture_max_per_tenant: 5_000,
            plan_capture_max_per_source: 50,
            plan_capture_max_age_days: 30,
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

impl Default for TenantLimitsConfig {
    fn default() -> Self {
        let defaults = sift_protocol::TenantResourceLimits {
            connection_profiles: Some(100),
            sessions: Some(32),
            connections: Some(64),
            concurrent_queries: Some(16),
            cursors: Some(64),
            retained_result_bytes: Some(256 * 1024 * 1024),
        };
        Self {
            trusted_local_unlimited: true,
            ceilings: defaults.clone(),
            defaults,
        }
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

/// Load one explicit config file over defaults. Remote bootstrap uses this so
/// it never depends on the SSH command's working directory.
pub fn load_path(path: impl AsRef<std::path::Path>) -> anyhow::Result<Config> {
    use figment::providers::{Format, Toml};
    let fig = figment::Figment::new()
        .merge(figment::providers::Serialized::defaults(Config::default()))
        .merge(Toml::file(path.as_ref()));
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
        assert_eq!(config.mode, RuntimeMode::InProcess);
        config.validate().unwrap();
    }

    #[test]
    fn runtime_modes_parse_without_changing_transport_policy() {
        assert_eq!("in-process".parse(), Ok(RuntimeMode::InProcess));
        assert_eq!("daemon".parse(), Ok(RuntimeMode::Daemon));
        assert_eq!("container".parse(), Ok(RuntimeMode::Container));
        assert!("background".parse::<RuntimeMode>().is_err());
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
    fn team_requires_durable_metadata_and_ssh_requires_daemon_mode() {
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
            mode: RuntimeMode::Daemon,
            auth: AuthConfig {
                loopback_bypass: false,
                ..AuthConfig::default()
            },
            metadata: MetadataConfig {
                secret_backend: "file".into(),
                ..MetadataConfig::default()
            },
            ..Config::default()
        };
        ssh.validate().unwrap();

        let foreground_ssh = Config {
            transport: Transport::SshProxy,
            auth: AuthConfig {
                loopback_bypass: false,
                ..AuthConfig::default()
            },
            metadata: MetadataConfig {
                secret_backend: "file".into(),
                ..MetadataConfig::default()
            },
            ..Config::default()
        };
        assert!(foreground_ssh
            .validate()
            .unwrap_err()
            .to_string()
            .contains("mode=daemon"));

        let container_ssh = Config {
            transport: Transport::SshProxy,
            mode: RuntimeMode::Container,
            auth: AuthConfig {
                loopback_bypass: false,
                ..AuthConfig::default()
            },
            metadata: MetadataConfig {
                secret_backend: "file".into(),
                ..MetadataConfig::default()
            },
            ..Config::default()
        };
        assert!(container_ssh.validate().is_err());
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
