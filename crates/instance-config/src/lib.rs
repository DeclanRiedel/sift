//! Pure data model for reproducible Sift server instances.
//!
//! This crate parses, normalizes, validates, and locks operator-authored
//! configuration. It performs no filesystem, network, database, secret-store,
//! process, or operating-system access.

use std::collections::{BTreeMap, BTreeSet};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

pub const MANIFEST_KIND: &str = "sift-instance";
pub const LOCK_KIND: &str = "sift-lock";
pub const FORMAT_VERSION: u32 = 1;
pub const PROVIDER_SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_CONNECTION_STRING_BYTES: usize = 16 * 1024;
const MAX_RESOURCES: usize = 10_000;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit")]
    ManifestTooLarge,
    #[error("invalid manifest TOML: {0}")]
    ManifestToml(String),
    #[error("invalid lock TOML: {0}")]
    LockToml(String),
    #[error("manifest validation failed at {path}: {message}")]
    Validation { path: String, message: String },
    #[error("lock verification failed: {0}")]
    Lock(String),
    #[error("unable to serialize normalized configuration: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub kind: String,
    pub format_version: u32,
    pub manifest_id: Uuid,
    pub name: String,
    pub compatibility: Compatibility,
    pub server: ServerConfig,
    pub automation: AutomationConfig,
    pub auth: AuthConfig,
    pub identity: IdentityConfig,
    pub tenants: Vec<TenantConfig>,
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
    #[serde(default)]
    pub extensions: Vec<ExtensionConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub sift: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub deployment: Deployment,
    pub transport: Transport,
    pub mode: RuntimeMode,
    pub bind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
    #[serde(default)]
    pub metadata: MetadataConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default, skip_serializing_if = "WorkspaceConfig::is_default")]
    pub workspaces: WorkspaceConfig,
    #[serde(default, skip_serializing_if = "VcsConfig::is_default")]
    pub vcs: VcsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub enabled: bool,
    pub roots: Vec<WorkspaceRootConfig>,
}

impl WorkspaceConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRootConfig {
    pub handle: String,
    pub path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VcsConfig {
    pub enabled: bool,
    pub network_enabled: bool,
    pub executable: Option<String>,
    pub local_timeout_secs: u64,
    pub network_timeout_secs: u64,
    pub max_output_bytes: u64,
    pub max_file_bytes: u64,
    pub max_status_entries: u32,
    pub max_history_page: u32,
    pub max_commit_files: u32,
    pub max_diff_files: u32,
    pub max_diff_hunks: u32,
    pub max_diff_lines: u32,
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

impl VcsConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Deployment {
    Personal,
    Team,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Loopback,
    Network,
    SshProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    InProcess,
    Daemon,
    Container,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeoutConfig {
    pub request_secs: u64,
    pub shutdown_drain_secs: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            request_secs: 30,
            shutdown_drain_secs: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetadataConfig {
    pub secret_backend: SecretBackend,
    pub store_sql: bool,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            secret_backend: SecretBackend::File,
            store_sql: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretBackend {
    File,
    Keychain,
    Memory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_http_result_rows: u64,
    pub max_http_result_bytes: u64,
    pub max_connections: u32,
    pub max_concurrent_queries: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_http_result_rows: 10_000,
            max_http_result_bytes: 16 * 1024 * 1024,
            max_connections: 64,
            max_concurrent_queries: 16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationConfig {
    pub unattended_apply: UnattendedApply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnattendedApply {
    Disabled,
    StandardRisk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    pub github: GithubAuthConfig,
    pub admission: AdmissionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubAuthConfig {
    pub flow: GithubFlow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubFlow {
    LocalDevice,
    HostedCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionConfig {
    pub mode: AdmissionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdmissionMode {
    Allowlist,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    pub github_principals: Vec<GithubPrincipal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubPrincipal {
    pub name: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_hint: Option<String>,
    #[serde(default)]
    pub instance_admin: bool,
    #[serde(default)]
    pub bootstrap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantConfig {
    pub name: String,
    pub memberships: Vec<TenantMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantMembership {
    pub principal: String,
    pub role: TenantRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TenantRole {
    Owner,
    Editor,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub name: String,
    pub tenant: String,
    pub provider: Provider,
    pub connection_string: String,
    pub credential_mode: CredentialMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub policy: ConnectionPolicy,
    #[serde(default)]
    pub lifecycle: ResourceLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Postgres,
    SqlServer,
}

impl Provider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::SqlServer => "sql-server",
        }
    }
}

impl ConnectionConfig {
    /// Convert the credential-free connection string into the built-in
    /// provider's typed public configuration. Secret fields are never added.
    pub fn provider_configuration(&self) -> Result<serde_json::Value, ConfigError> {
        validate_connection_string("connection_string", self.provider, &self.connection_string)?;
        match self.provider {
            Provider::Postgres => postgres_provider_configuration(&self.connection_string),
            Provider::SqlServer => sql_server_provider_configuration(&self.connection_string),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialMode {
    Shared,
    PerUser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectionPolicy {
    pub allow_sql: bool,
    pub allow_schema_read: bool,
    pub allow_export: bool,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            allow_sql: true,
            allow_schema_read: true,
            allow_export: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLifecycle {
    pub prevent_destroy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionConfig {
    pub name: String,
    pub version: String,
    pub artifact: String,
    pub sha256: String,
    pub publisher_key: String,
    #[serde(default)]
    pub grants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockFile {
    pub kind: String,
    pub format_version: u32,
    pub manifest_id: Uuid,
    pub configuration_digest: String,
    pub sift: SiftLock,
    pub providers: Vec<ProviderLock>,
    #[serde(default)]
    pub extensions: Vec<ExtensionLock>,
    #[serde(default)]
    pub platforms: Vec<PlatformLock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiftLock {
    pub version: String,
    pub protocol: u32,
    pub schema_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderLock {
    pub name: Provider,
    pub schema_version: u32,
    pub schema_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionLock {
    pub name: String,
    pub version: String,
    pub artifact: String,
    pub sha256: String,
    pub publisher_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformLock {
    pub target: String,
    pub sift_artifact: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticPlan {
    pub manifest_id: Uuid,
    pub configuration_digest: String,
    pub lock_digest: String,
    pub resources: ResourceCounts,
    pub required_credentials: Vec<CredentialRequirement>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCounts {
    pub principals: usize,
    pub tenants: usize,
    pub memberships: usize,
    pub connections: usize,
    pub extensions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequirement {
    pub slot: String,
    pub consumer: String,
    pub kind: CredentialKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialKind {
    GithubOauthClientSecret,
    Postgres,
    SqlServer,
}

impl Manifest {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        if input.len() > MAX_MANIFEST_BYTES {
            return Err(ConfigError::ManifestTooLarge);
        }
        let mut manifest: Self = toml::from_str(input)
            .map_err(|error: toml::de::Error| ConfigError::ManifestToml(error.message().into()))?;
        manifest.normalize();
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn normalize(&mut self) {
        self.name = self.name.trim().to_owned();
        self.server.bind = self.server.bind.trim().to_owned();
        self.server.public_base_url = self
            .server
            .public_base_url
            .take()
            .map(|value| value.trim().trim_end_matches('/').to_owned());
        self.compatibility.sift = self.compatibility.sift.trim().to_owned();
        self.auth.github.client_id = self
            .auth
            .github
            .client_id
            .take()
            .map(|value| value.trim().to_owned());
        for root in &mut self.server.workspaces.roots {
            root.handle = root.handle.trim().to_owned();
            root.path = root.path.trim().to_owned();
        }
        self.server
            .workspaces
            .roots
            .sort_by(|left, right| left.handle.cmp(&right.handle));
        self.auth.github.client_secret = self
            .auth
            .github
            .client_secret
            .take()
            .map(|value| value.trim().to_owned());
        for principal in &mut self.identity.github_principals {
            principal.name = principal.name.trim().to_owned();
            principal.subject = principal.subject.trim().to_owned();
            principal.login_hint = principal
                .login_hint
                .take()
                .map(|value| value.trim().to_owned());
        }
        for tenant in &mut self.tenants {
            tenant.name = tenant.name.trim().to_owned();
            for membership in &mut tenant.memberships {
                membership.principal = membership.principal.trim().to_owned();
            }
            tenant
                .memberships
                .sort_by(|left, right| left.principal.cmp(&right.principal));
        }
        self.tenants
            .sort_by(|left, right| left.name.cmp(&right.name));
        for connection in &mut self.connections {
            connection.name = connection.name.trim().to_owned();
            connection.tenant = connection.tenant.trim().to_owned();
            connection.connection_string = connection.connection_string.trim().to_owned();
            connection.credential = connection
                .credential
                .take()
                .map(|value| value.trim().to_owned());
            connection.tags.iter_mut().for_each(|tag| {
                *tag = tag.trim().to_owned();
            });
            connection.tags.sort();
            connection.tags.dedup();
        }
        self.connections
            .sort_by(|left, right| left.name.cmp(&right.name));
        for extension in &mut self.extensions {
            extension.name = extension.name.trim().to_owned();
            extension.version = extension.version.trim().to_owned();
            extension.artifact = extension.artifact.trim().to_owned();
            extension.sha256 = extension.sha256.trim().to_ascii_lowercase();
            extension.publisher_key = extension.publisher_key.trim().to_owned();
            extension.grants.iter_mut().for_each(|grant| {
                *grant = grant.trim().to_owned();
            });
            extension.grants.sort();
            extension.grants.dedup();
        }
        self.extensions
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.kind != MANIFEST_KIND {
            return validation("kind", format!("must be `{MANIFEST_KIND}`"));
        }
        if self.format_version != FORMAT_VERSION {
            return validation(
                "format_version",
                format!(
                    "unsupported version {}; expected {FORMAT_VERSION}",
                    self.format_version
                ),
            );
        }
        if self.manifest_id.is_nil() {
            return validation("manifest_id", "must not be the nil UUID");
        }
        validate_logical_name("name", &self.name)?;
        VersionReq::parse(&self.compatibility.sift).map_err(|_| ConfigError::Validation {
            path: "compatibility.sift".into(),
            message: "must be a valid semantic-version requirement".into(),
        })?;
        self.server.validate()?;
        self.auth.validate(&self.server)?;

        let resource_count = self
            .identity
            .github_principals
            .len()
            .saturating_add(self.tenants.len())
            .saturating_add(self.connections.len())
            .saturating_add(self.extensions.len());
        if resource_count > MAX_RESOURCES {
            return validation(
                "resources",
                format!("exceeds the {MAX_RESOURCES}-resource limit"),
            );
        }

        let mut principals = BTreeSet::new();
        let mut subjects = BTreeSet::new();
        let mut bootstrap_count = 0;
        for (index, principal) in self.identity.github_principals.iter().enumerate() {
            let base = format!("identity.github_principals[{index}]");
            validate_logical_name(&format!("{base}.name"), &principal.name)?;
            if !principals.insert(principal.name.as_str()) {
                return validation(format!("{base}.name"), "duplicate principal name");
            }
            if principal.subject.is_empty()
                || !principal.subject.bytes().all(|byte| byte.is_ascii_digit())
                || principal.subject.starts_with('0')
            {
                return validation(
                    format!("{base}.subject"),
                    "must be an immutable positive decimal GitHub user id",
                );
            }
            if !subjects.insert(principal.subject.as_str()) {
                return validation(format!("{base}.subject"), "duplicate GitHub subject");
            }
            if principal.bootstrap {
                bootstrap_count += 1;
                if !principal.instance_admin {
                    return validation(
                        format!("{base}.instance_admin"),
                        "bootstrap principal must be an instance administrator",
                    );
                }
            }
            if let Some(hint) = &principal.login_hint {
                validate_display_hint(&format!("{base}.login_hint"), hint)?;
            }
        }
        if bootstrap_count != 1 {
            return validation(
                "identity.github_principals",
                "must contain exactly one bootstrap principal",
            );
        }

        let mut tenants = BTreeSet::new();
        for (tenant_index, tenant) in self.tenants.iter().enumerate() {
            let base = format!("tenants[{tenant_index}]");
            validate_logical_name(&format!("{base}.name"), &tenant.name)?;
            if !tenants.insert(tenant.name.as_str()) {
                return validation(format!("{base}.name"), "duplicate tenant name");
            }
            let mut memberships = BTreeSet::new();
            for (membership_index, membership) in tenant.memberships.iter().enumerate() {
                let path = format!("{base}.memberships[{membership_index}].principal");
                if !principals.contains(membership.principal.as_str()) {
                    return validation(path, "references an unknown principal");
                }
                if !memberships.insert(membership.principal.as_str()) {
                    return validation(path, "duplicate tenant membership");
                }
            }
            if !tenant.memberships.iter().any(|membership| {
                membership.role == TenantRole::Owner
                    && self
                        .identity
                        .github_principals
                        .iter()
                        .any(|principal| principal.name == membership.principal)
            }) {
                return validation(format!("{base}.memberships"), "requires at least one owner");
            }
        }
        if self.tenants.is_empty() {
            return validation("tenants", "requires at least one tenant");
        }

        let mut connections = BTreeSet::new();
        let mut credential_kinds = BTreeMap::new();
        for (index, connection) in self.connections.iter().enumerate() {
            let base = format!("connections[{index}]");
            validate_logical_name(&format!("{base}.name"), &connection.name)?;
            if !connections.insert(connection.name.as_str()) {
                return validation(format!("{base}.name"), "duplicate connection name");
            }
            if !tenants.contains(connection.tenant.as_str()) {
                return validation(format!("{base}.tenant"), "references an unknown tenant");
            }
            validate_connection_string(
                &format!("{base}.connection_string"),
                connection.provider,
                &connection.connection_string,
            )?;
            match (connection.credential_mode, connection.credential.as_deref()) {
                (CredentialMode::Shared, Some(slot)) => {
                    validate_credential_ref(&format!("{base}.credential"), slot)?;
                    if let Some(previous) = credential_kinds.insert(slot, connection.provider) {
                        if previous != connection.provider {
                            return validation(
                                format!("{base}.credential"),
                                "a credential slot cannot be shared by different provider kinds",
                            );
                        }
                    }
                }
                (CredentialMode::Shared, None) => {
                    return validation(
                        format!("{base}.credential"),
                        "shared mode requires a credential slot reference",
                    )
                }
                (CredentialMode::PerUser, Some(_)) => {
                    return validation(
                        format!("{base}.credential"),
                        "per-user mode cannot declare a shared credential slot",
                    )
                }
                (CredentialMode::PerUser, None) => {}
            }
            if !connection.enabled {
                return validation(
                    format!("{base}.enabled"),
                    "format v1 does not silently realize disabled connections; remove the resource instead",
                );
            }
            for (tag_index, tag) in connection.tags.iter().enumerate() {
                validate_logical_name(&format!("{base}.tags[{tag_index}]"), tag)?;
            }
        }

        let mut extensions = BTreeSet::new();
        for (index, extension) in self.extensions.iter().enumerate() {
            let base = format!("extensions[{index}]");
            validate_logical_name(&format!("{base}.name"), &extension.name)?;
            if !extensions.insert(extension.name.as_str()) {
                return validation(format!("{base}.name"), "duplicate extension name");
            }
            Version::parse(&extension.version).map_err(|_| ConfigError::Validation {
                path: format!("{base}.version"),
                message: "must be an exact semantic version".into(),
            })?;
            validate_https_url(&format!("{base}.artifact"), &extension.artifact)?;
            validate_sha256(&format!("{base}.sha256"), &extension.sha256)?;
            if extension.publisher_key.is_empty() || extension.publisher_key.len() > 512 {
                return validation(
                    format!("{base}.publisher_key"),
                    "must be a non-empty bounded publisher key id",
                );
            }
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ConfigError> {
        let mut normalized = self.clone();
        normalized.normalize();
        normalized.validate()?;
        serde_json::to_vec(&normalized)
            .map_err(|error| ConfigError::Serialization(error.to_string()))
    }

    pub fn configuration_digest(&self) -> Result<String, ConfigError> {
        Ok(sha256_prefixed(&self.canonical_bytes()?))
    }

    pub fn to_toml_pretty(&self) -> Result<String, ConfigError> {
        let mut normalized = self.clone();
        normalized.normalize();
        normalized.validate()?;
        toml::to_string_pretty(&normalized)
            .map_err(|error| ConfigError::Serialization(error.to_string()))
    }

    pub fn static_plan(&self, lock: &LockFile) -> Result<StaticPlan, ConfigError> {
        lock.verify(self)?;
        let mut requirements = Vec::new();
        if let Some(slot) = &self.auth.github.client_secret {
            requirements.push(CredentialRequirement {
                slot: slot.clone(),
                consumer: "auth.github".into(),
                kind: CredentialKind::GithubOauthClientSecret,
            });
        }
        for connection in &self.connections {
            if let Some(slot) = &connection.credential {
                requirements.push(CredentialRequirement {
                    slot: slot.clone(),
                    consumer: format!("connection.{}", connection.name),
                    kind: match connection.provider {
                        Provider::Postgres => CredentialKind::Postgres,
                        Provider::SqlServer => CredentialKind::SqlServer,
                    },
                });
            }
        }
        requirements.sort_by(|left, right| {
            left.slot
                .cmp(&right.slot)
                .then_with(|| left.consumer.cmp(&right.consumer))
        });
        let memberships = self
            .tenants
            .iter()
            .map(|tenant| tenant.memberships.len())
            .sum();
        let warnings = if self.server.metadata.secret_backend == SecretBackend::Memory {
            vec!["memory secret backend is non-durable".into()]
        } else {
            Vec::new()
        };
        Ok(StaticPlan {
            manifest_id: self.manifest_id,
            configuration_digest: self.configuration_digest()?,
            lock_digest: lock.digest()?,
            resources: ResourceCounts {
                principals: self.identity.github_principals.len(),
                tenants: self.tenants.len(),
                memberships,
                connections: self.connections.len(),
                extensions: self.extensions.len(),
            },
            required_credentials: requirements,
            warnings,
        })
    }
}

impl ServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        let bind = self.bind.as_str();
        let symbolic = matches!(bind, "auto-loopback" | "prompt-required");
        if !symbolic {
            bind.parse::<std::net::SocketAddr>()
                .map_err(|_| ConfigError::Validation {
                    path: "server.bind".into(),
                    message: "must be a socket address or a supported symbolic binding".into(),
                })?;
        }
        if self.transport == Transport::Loopback
            && !symbolic
            && !bind
                .parse::<std::net::SocketAddr>()
                .expect("validated socket address")
                .ip()
                .is_loopback()
        {
            return validation(
                "server.bind",
                "loopback transport requires a loopback address",
            );
        }
        if self.transport == Transport::SshProxy && self.mode != RuntimeMode::Daemon {
            return validation("server.mode", "ssh-proxy transport requires daemon mode");
        }
        if self.mode == RuntimeMode::Container && self.transport == Transport::SshProxy {
            return validation("server.transport", "container mode cannot use ssh-proxy");
        }
        if self.metadata.secret_backend == SecretBackend::Memory {
            return validation(
                "server.metadata.secret_backend",
                "instance credentials require a durable file or keychain backend",
            );
        }
        if let Some(origin) = &self.public_base_url {
            validate_https_origin("server.public_base_url", origin)?;
        }
        if self.deployment == Deployment::Team && self.public_base_url.is_none() {
            return validation(
                "server.public_base_url",
                "team deployment requires an explicit HTTPS origin",
            );
        }
        if self.timeouts.request_secs == 0 || self.timeouts.request_secs > 300 {
            return validation("server.timeouts.request_secs", "must be between 1 and 300");
        }
        if self.timeouts.shutdown_drain_secs > 3_600 {
            return validation(
                "server.timeouts.shutdown_drain_secs",
                "must not exceed 3600",
            );
        }
        if self.limits.max_http_result_rows == 0
            || self.limits.max_http_result_rows > 1_000_000
            || self.limits.max_http_result_bytes == 0
            || self.limits.max_http_result_bytes > 1024 * 1024 * 1024
            || self.limits.max_connections == 0
            || self.limits.max_connections > 10_000
            || self.limits.max_concurrent_queries == 0
            || self.limits.max_concurrent_queries > self.limits.max_connections
        {
            return validation("server.limits", "contains an unsafe or invalid limit");
        }
        if self.workspaces.enabled && self.workspaces.roots.is_empty() {
            return validation(
                "server.workspaces.roots",
                "requires at least one root when workspace projections are enabled",
            );
        }
        if self.vcs.enabled && !self.workspaces.enabled {
            return validation(
                "server.vcs.enabled",
                "requires workspace projections to be enabled",
            );
        }
        if self.vcs.network_enabled && !self.vcs.enabled {
            return validation(
                "server.vcs.network_enabled",
                "requires VCS integration to be enabled",
            );
        }
        if self
            .vcs
            .executable
            .as_deref()
            .is_some_and(|path| !std::path::Path::new(path).is_absolute())
        {
            return validation("server.vcs.executable", "must be an absolute path");
        }
        if !(1..=300).contains(&self.vcs.local_timeout_secs)
            || !(1..=900).contains(&self.vcs.network_timeout_secs)
            || !(1..=64 * 1024 * 1024).contains(&self.vcs.max_output_bytes)
            || !(1..=64 * 1024 * 1024).contains(&self.vcs.max_file_bytes)
            || !(1..=100_000).contains(&self.vcs.max_status_entries)
            || !(1..=1_000).contains(&self.vcs.max_history_page)
            || !(1..=25_000).contains(&self.vcs.max_commit_files)
            || !(1..=10_000).contains(&self.vcs.max_diff_files)
            || !(1..=20_000).contains(&self.vcs.max_diff_hunks)
            || !(1..=1_000_000).contains(&self.vcs.max_diff_lines)
        {
            return validation(
                "server.vcs",
                "contains an unsafe timeout, output, file, status, history, commit, or diff limit",
            );
        }
        let mut root_handles = BTreeSet::new();
        let mut root_paths = BTreeSet::new();
        for (index, root) in self.workspaces.roots.iter().enumerate() {
            let base = format!("server.workspaces.roots[{index}]");
            validate_logical_name(&format!("{base}.handle"), &root.handle)?;
            if !root_handles.insert(root.handle.as_str()) {
                return validation(format!("{base}.handle"), "duplicate workspace root handle");
            }
            if !std::path::Path::new(&root.path).is_absolute() {
                return validation(format!("{base}.path"), "must be an absolute path");
            }
            if !root_paths.insert(root.path.as_str()) {
                return validation(format!("{base}.path"), "duplicate workspace root path");
            }
        }
        Ok(())
    }
}

impl AuthConfig {
    fn validate(&self, server: &ServerConfig) -> Result<(), ConfigError> {
        match self.github.flow {
            GithubFlow::LocalDevice => {
                if server.deployment != Deployment::Personal
                    || server.transport != Transport::Loopback
                {
                    return validation(
                        "auth.github.flow",
                        "local-device is limited to personal loopback servers",
                    );
                }
                if self.github.client_secret.is_some() {
                    return validation(
                        "auth.github.client_secret",
                        "local-device flow cannot declare a client-secret slot",
                    );
                }
            }
            GithubFlow::HostedCode => {
                let client_id = self.github.client_id.as_deref().unwrap_or_default();
                if client_id.is_empty() || client_id.len() > 256 {
                    return validation(
                        "auth.github.client_id",
                        "hosted-code requires a bounded client id",
                    );
                }
                let slot = self.github.client_secret.as_deref().unwrap_or_default();
                validate_credential_ref("auth.github.client_secret", slot)?;
                if server.public_base_url.is_none() {
                    return validation(
                        "server.public_base_url",
                        "hosted-code requires an HTTPS public origin",
                    );
                }
            }
        }
        Ok(())
    }
}

impl LockFile {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        toml::from_str(input)
            .map_err(|error: toml::de::Error| ConfigError::LockToml(error.message().into()))
    }

    pub fn generate(
        manifest: &Manifest,
        sift_version: &str,
        protocol: u32,
    ) -> Result<Self, ConfigError> {
        manifest.validate()?;
        let version = Version::parse(sift_version)
            .map_err(|_| ConfigError::Lock("Sift version is not valid semver".into()))?;
        let requirement = VersionReq::parse(&manifest.compatibility.sift)
            .map_err(|_| ConfigError::Lock("manifest Sift requirement is invalid".into()))?;
        if !requirement.matches(&version) {
            return Err(ConfigError::Lock(format!(
                "Sift {version} does not satisfy {}",
                manifest.compatibility.sift
            )));
        }
        let providers = manifest
            .connections
            .iter()
            .map(|connection| connection.provider)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|name| ProviderLock {
                name,
                schema_version: PROVIDER_SCHEMA_VERSION,
                schema_digest: provider_schema_digest(name),
            })
            .collect();
        let extensions = manifest
            .extensions
            .iter()
            .map(|extension| ExtensionLock {
                name: extension.name.clone(),
                version: extension.version.clone(),
                artifact: extension.artifact.clone(),
                sha256: extension.sha256.clone(),
                publisher_key: extension.publisher_key.clone(),
            })
            .collect();
        let mut lock = Self {
            kind: LOCK_KIND.into(),
            format_version: FORMAT_VERSION,
            manifest_id: manifest.manifest_id,
            configuration_digest: manifest.configuration_digest()?,
            sift: SiftLock {
                version: version.to_string(),
                protocol,
                schema_digest: String::new(),
            },
            providers,
            extensions,
            platforms: Vec::new(),
        };
        lock.sift.schema_digest = lock_schema_digest(&lock);
        Ok(lock)
    }

    pub fn verify(&self, manifest: &Manifest) -> Result<(), ConfigError> {
        manifest.validate()?;
        if self.kind != LOCK_KIND {
            return Err(ConfigError::Lock(format!("kind must be `{LOCK_KIND}`")));
        }
        if self.format_version != FORMAT_VERSION {
            return Err(ConfigError::Lock(format!(
                "unsupported format version {}",
                self.format_version
            )));
        }
        if self.manifest_id != manifest.manifest_id {
            return Err(ConfigError::Lock("manifest id does not match".into()));
        }
        if self.configuration_digest != manifest.configuration_digest()? {
            return Err(ConfigError::Lock(
                "configuration digest does not match".into(),
            ));
        }
        Version::parse(&self.sift.version)
            .map_err(|_| ConfigError::Lock("locked Sift version is invalid".into()))?;
        let expected_schema = lock_schema_digest(self);
        if self.sift.schema_digest != expected_schema {
            return Err(ConfigError::Lock(
                "Sift schema digest does not match".into(),
            ));
        }
        let expected_providers = manifest
            .connections
            .iter()
            .map(|connection| connection.provider)
            .collect::<BTreeSet<_>>();
        let actual_providers = self
            .providers
            .iter()
            .map(|provider| provider.name)
            .collect::<BTreeSet<_>>();
        if self.providers.len() != actual_providers.len() || expected_providers != actual_providers
        {
            return Err(ConfigError::Lock(
                "provider closure does not match manifest".into(),
            ));
        }
        for provider in &self.providers {
            if provider.schema_version != PROVIDER_SCHEMA_VERSION
                || provider.schema_digest != provider_schema_digest(provider.name)
            {
                return Err(ConfigError::Lock(format!(
                    "provider schema lock does not match for {}",
                    provider.name.as_str()
                )));
            }
        }
        let expected_extensions = manifest
            .extensions
            .iter()
            .map(|extension| {
                (
                    &extension.name,
                    &extension.version,
                    &extension.artifact,
                    &extension.sha256,
                    &extension.publisher_key,
                )
            })
            .collect::<Vec<_>>();
        let actual_extensions = self
            .extensions
            .iter()
            .map(|extension| {
                (
                    &extension.name,
                    &extension.version,
                    &extension.artifact,
                    &extension.sha256,
                    &extension.publisher_key,
                )
            })
            .collect::<Vec<_>>();
        if expected_extensions != actual_extensions {
            return Err(ConfigError::Lock(
                "extension closure does not match manifest".into(),
            ));
        }
        validate_unique_platforms(&self.platforms)?;
        Ok(())
    }

    /// Verify that the process interpreting this lock is the exact Sift and
    /// protocol version selected by it. Structural verification alone is not
    /// sufficient for reproducible startup.
    pub fn verify_runtime(
        &self,
        manifest: &Manifest,
        sift_version: &str,
        protocol: u32,
    ) -> Result<(), ConfigError> {
        self.verify(manifest)?;
        let running = Version::parse(sift_version)
            .map_err(|_| ConfigError::Lock("running Sift version is invalid".into()))?;
        let locked = Version::parse(&self.sift.version)
            .map_err(|_| ConfigError::Lock("locked Sift version is invalid".into()))?;
        if running != locked {
            return Err(ConfigError::Lock(format!(
                "running Sift {running} does not match locked Sift {locked}"
            )));
        }
        if self.sift.protocol != protocol {
            return Err(ConfigError::Lock(format!(
                "running protocol {protocol} does not match locked protocol {}",
                self.sift.protocol
            )));
        }
        let requirement = VersionReq::parse(&manifest.compatibility.sift)
            .map_err(|_| ConfigError::Lock("manifest Sift requirement is invalid".into()))?;
        if !requirement.matches(&running) {
            return Err(ConfigError::Lock(format!(
                "running Sift {running} does not satisfy {}",
                manifest.compatibility.sift
            )));
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, ConfigError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ConfigError::Serialization(error.to_string()))?;
        Ok(sha256_prefixed(&bytes))
    }

    pub fn to_toml_pretty(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|error| ConfigError::Serialization(error.to_string()))
    }
}

fn default_true() -> bool {
    true
}

fn validation<T>(path: impl Into<String>, message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Validation {
        path: path.into(),
        message: message.into(),
    })
}

fn validate_logical_name(path: &str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty() || value.len() > 128 || value.starts_with('/') || value.ends_with('/') {
        return validation(
            path,
            "must be a non-empty logical name of at most 128 bytes",
        );
    }
    for segment in value.split('/') {
        let mut bytes = segment.bytes();
        let Some(first) = bytes.next() else {
            return validation(path, "must not contain an empty path segment");
        };
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return validation(path, "segments must start with a lowercase letter or digit");
        }
        if !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        }) {
            return validation(
                path,
                "may contain only lowercase ASCII letters, digits, '-', '_', '.', and '/'",
            );
        }
    }
    Ok(())
}

fn validate_display_hint(path: &str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(|character| character.is_control())
    {
        return validation(path, "must be non-empty, bounded, and contain no controls");
    }
    Ok(())
}

fn validate_credential_ref(path: &str, value: &str) -> Result<(), ConfigError> {
    let Some(name) = value.strip_prefix("credential:") else {
        return validation(path, "must be a credential: logical slot reference");
    };
    validate_logical_name(path, name)
}

fn validate_connection_string(
    path: &str,
    provider: Provider,
    value: &str,
) -> Result<(), ConfigError> {
    if value.is_empty() || value.len() > MAX_CONNECTION_STRING_BYTES {
        return validation(path, "must be non-empty and at most 16384 bytes");
    }
    if value.chars().any(|character| character.is_control()) {
        return validation(path, "must not contain control characters");
    }
    match provider {
        Provider::Postgres => validate_postgres_connection_string(path, value),
        Provider::SqlServer => validate_sql_server_connection_string(path, value),
    }
}

fn validate_postgres_connection_string(path: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = Url::parse(value).map_err(|_| ConfigError::Validation {
        path: path.into(),
        message: "must be a valid PostgreSQL URL".into(),
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") || parsed.host_str().is_none() {
        return validation(path, "must use postgres:// or postgresql:// with a host");
    }
    if parsed.password().is_some() {
        return validation(path, "must not contain an inline password");
    }
    if parsed.username().is_empty() {
        return validation(path, "must contain a database username");
    }
    if parsed.path().trim_matches('/').is_empty() {
        return validation(path, "must contain a database name");
    }
    if parsed.fragment().is_some() {
        return validation(path, "must not contain a fragment");
    }
    for (key, value) in parsed.query_pairs() {
        if secret_key_name(&key) {
            return validation(path, "must not contain secret-bearing query parameters");
        }
        if key != "sslmode" {
            return validation(path, "contains an unsupported PostgreSQL connection option");
        }
        if !matches!(
            value.as_ref(),
            "disable" | "prefer" | "require" | "verify-ca" | "verify-full"
        ) {
            return validation(path, "contains an invalid PostgreSQL sslmode");
        }
    }
    Ok(())
}

fn validate_sql_server_connection_string(path: &str, value: &str) -> Result<(), ConfigError> {
    if value.contains(['{', '}', '\"', '\'']) {
        return validation(
            path,
            "v1 accepts only unquoted key=value SQL Server connection fields",
        );
    }
    let mut fields = BTreeMap::new();
    for part in value.split(';').filter(|part| !part.trim().is_empty()) {
        let Some((key, field_value)) = part.split_once('=') else {
            return validation(path, "SQL Server fields must use key=value syntax");
        };
        let key = key.trim().to_ascii_lowercase().replace(' ', "");
        if key.is_empty() || field_value.trim().is_empty() {
            return validation(path, "SQL Server fields require a key and value");
        }
        if secret_key_name(&key) {
            return validation(path, "must not contain secret-bearing fields");
        }
        if fields.insert(key, field_value.trim()).is_some() {
            return validation(path, "must not contain duplicate SQL Server fields");
        }
    }
    if !fields.contains_key("server") && !fields.contains_key("datasource") {
        return validation(
            path,
            "SQL Server connection string requires Server or Data Source",
        );
    }
    if !fields.contains_key("userid") && !fields.contains_key("uid") {
        return validation(path, "SQL Server connection string requires User ID or UID");
    }
    for key in fields.keys() {
        if !matches!(
            key.as_str(),
            "server"
                | "datasource"
                | "database"
                | "initialcatalog"
                | "userid"
                | "uid"
                | "encrypt"
                | "trustservercertificate"
                | "mars"
        ) {
            return validation(path, "contains an unsupported SQL Server connection field");
        }
    }
    Ok(())
}

fn postgres_provider_configuration(value: &str) -> Result<serde_json::Value, ConfigError> {
    let parsed = Url::parse(value).map_err(|_| ConfigError::Validation {
        path: "connection_string".into(),
        message: "must be a valid PostgreSQL URL".into(),
    })?;
    let ssl_mode = parsed
        .query_pairs()
        .find(|(key, _)| key == "sslmode")
        .map(|(_, value)| value.replace('-', "_"));
    Ok(serde_json::json!({
        "host": parsed.host_str().expect("validated PostgreSQL host"),
        "port": parsed.port().unwrap_or(5432),
        "database": parsed.path().trim_start_matches('/'),
        "user": parsed.username(),
        "password": null,
        "ssl_mode": ssl_mode,
        "engine_specific": {
            "engine": "postgres",
            "search_path": null,
            "application_name": "sift",
            "connect_timeout_secs": null,
            "pool_max_size": null,
            "pool_min_size": null
        }
    }))
}

fn sql_server_provider_configuration(value: &str) -> Result<serde_json::Value, ConfigError> {
    let fields = value
        .split(';')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').expect("validated SQL Server field");
            (
                key.trim().to_ascii_lowercase().replace(' ', ""),
                value.trim(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let server = fields
        .get("server")
        .or_else(|| fields.get("datasource"))
        .expect("validated SQL Server host");
    let (host, port) = match server.rsplit_once(',') {
        Some((host, port)) => {
            let port = port.parse::<u16>().map_err(|_| ConfigError::Validation {
                path: "connection_string".into(),
                message: "SQL Server port must be between 1 and 65535".into(),
            })?;
            (host, Some(port))
        }
        None => (*server, None),
    };
    if host.is_empty() || host.contains('\\') {
        return validation(
            "connection_string",
            "v1 SQL Server endpoints require a host with optional comma port",
        );
    }
    let parse_bool = |key: &str| -> Result<Option<bool>, ConfigError> {
        fields
            .get(key)
            .map(|value| match value.to_ascii_lowercase().as_str() {
                "true" | "yes" => Ok(true),
                "false" | "no" => Ok(false),
                _ => validation("connection_string", format!("{key} must be true or false")),
            })
            .transpose()
    };
    Ok(serde_json::json!({
        "host": host,
        "port": port.unwrap_or(1433),
        "database": fields.get("database").or_else(|| fields.get("initialcatalog")),
        "user": fields.get("userid").or_else(|| fields.get("uid")).expect("validated SQL Server user"),
        "password": null,
        "ssl_mode": null,
        "engine_specific": {
            "engine": "sql_server",
            "mars": parse_bool("mars")?.unwrap_or(false),
            "encrypt": parse_bool("encrypt")?,
            "trust_server_certificate": parse_bool("trustservercertificate")?,
            "connect_timeout_secs": null,
            "pool_min_size": null
        }
    }))
}

fn secret_key_name(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "password"
            | "pass"
            | "pwd"
            | "token"
            | "accesstoken"
            | "clientsecret"
            | "privatekey"
            | "sslkey"
            | "secret"
    ) || normalized.ends_with("password")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
}

fn validate_https_origin(path: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = Url::parse(value).map_err(|_| ConfigError::Validation {
        path: path.into(),
        message: "must be a valid HTTPS origin".into(),
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return validation(
            path,
            "must be an HTTPS origin without credentials, path, query, or fragment",
        );
    }
    Ok(())
}

fn validate_https_url(path: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = Url::parse(value).map_err(|_| ConfigError::Validation {
        path: path.into(),
        message: "must be a valid HTTPS URL".into(),
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return validation(path, "must be an HTTPS URL without credentials or fragment");
    }
    Ok(())
}

fn validate_sha256(path: &str, value: &str) -> Result<(), ConfigError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return validation(path, "must use sha256:<64 lowercase hex digits>");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return validation(path, "must use sha256:<64 lowercase hex digits>");
    }
    Ok(())
}

fn provider_schema_digest(provider: Provider) -> String {
    sha256_prefixed(
        format!(
            "sift-provider-schema:{}:{PROVIDER_SCHEMA_VERSION}",
            provider.as_str()
        )
        .as_bytes(),
    )
}

fn lock_schema_digest(lock: &LockFile) -> String {
    sha256_prefixed(
        format!(
            "sift-instance-schema:{}:{}:{}",
            lock.format_version, lock.sift.protocol, PROVIDER_SCHEMA_VERSION
        )
        .as_bytes(),
    )
}

fn validate_unique_platforms(platforms: &[PlatformLock]) -> Result<(), ConfigError> {
    let mut targets = BTreeSet::new();
    for (index, platform) in platforms.iter().enumerate() {
        if platform.target.is_empty() || !targets.insert(platform.target.as_str()) {
            return Err(ConfigError::Lock(format!(
                "platform target at index {index} is empty or duplicated"
            )));
        }
        validate_https_url("platform.sift_artifact", &platform.sift_artifact)?;
        validate_sha256("platform.sha256", &platform.sha256)?;
    }
    Ok(())
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(7 + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
kind = "sift-instance"
format_version = 1
manifest_id = "b654b918-b1f1-4d70-924d-e4c1014f482f"
name = "analytics-sift"

[compatibility]
sift = ">=0.1,<0.2"

[server]
deployment = "team"
transport = "network"
mode = "daemon"
bind = "0.0.0.0:7474"
public_base_url = "https://sift.example.test"

[server.metadata]
secret_backend = "file"
store_sql = false

[automation]
unattended_apply = "standard-risk"

[auth.github]
flow = "hosted-code"
client_id = "test-client"
client_secret = "credential:instance/github-oauth-client-secret"

[auth.admission]
mode = "allowlist"

[[identity.github_principals]]
name = "operator"
subject = "12345678"
login_hint = "operator"
instance_admin = true
bootstrap = true

[[tenants]]
name = "analytics"

[[tenants.memberships]]
principal = "operator"
role = "owner"

[[connections]]
name = "analytics/warehouse"
tenant = "analytics"
provider = "postgres"
connection_string = "postgresql://sift@warehouse.internal:5432/analytics?sslmode=verify-full"
credential_mode = "shared"
credential = "credential:analytics/warehouse/shared"
tags = ["warehouse", "production", "warehouse"]

[connections.policy]
allow_sql = true
allow_schema_read = true
allow_export = false

[connections.lifecycle]
prevent_destroy = true
"#;

    #[test]
    fn parses_normalizes_and_locks_manifest() {
        let manifest = Manifest::parse(VALID).unwrap();
        assert_eq!(manifest.connections[0].tags, ["production", "warehouse"]);
        let lock = LockFile::generate(&manifest, "0.1.0", 1).unwrap();
        lock.verify(&manifest).unwrap();
        assert_eq!(lock.providers.len(), 1);
        assert_eq!(
            manifest,
            Manifest::parse(&manifest.to_toml_pretty().unwrap()).unwrap()
        );
    }

    #[test]
    fn formatting_does_not_change_configuration_digest() {
        let manifest = Manifest::parse(VALID).unwrap();
        let formatted = format!("# comment\n{}", manifest.to_toml_pretty().unwrap());
        let reparsed = Manifest::parse(&formatted).unwrap();
        assert_eq!(
            manifest.configuration_digest().unwrap(),
            reparsed.configuration_digest().unwrap()
        );
    }

    #[test]
    fn workspace_and_vcs_configuration_is_validated_and_locked() {
        let input = VALID.replace(
            "[automation]",
            r#"[server.workspaces]
enabled = true

[[server.workspaces.roots]]
handle = " demo "
path = " /tmp/sift-demo "
read_only = false

[server.vcs]
enabled = true
network_enabled = false

[automation]"#,
        );
        let manifest = Manifest::parse(&input).unwrap();
        assert_eq!(manifest.server.workspaces.roots[0].handle, "demo");
        assert_eq!(manifest.server.workspaces.roots[0].path, "/tmp/sift-demo");
        assert!(manifest.server.vcs.enabled);

        let lock = LockFile::generate(&manifest, "0.1.0", 1).unwrap();
        lock.verify(&manifest).unwrap();
    }

    #[test]
    fn vcs_requires_an_enabled_workspace_root() {
        let input = VALID.replace(
            "[automation]",
            r#"[server.vcs]
enabled = true

[automation]"#,
        );
        let error = Manifest::parse(&input).unwrap_err().to_string();
        assert!(error.contains("server.vcs.enabled"));
        assert!(error.contains("requires workspace projections"));
    }

    #[test]
    fn unknown_fields_fail_closed_without_echoing_values() {
        let input = VALID.replace(
            "name = \"analytics-sift\"",
            "name = \"analytics-sift\"\npassword = \"do-not-echo-this\"",
        );
        let error = Manifest::parse(&input).unwrap_err().to_string();
        assert!(!error.contains("do-not-echo-this"));
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn connection_strings_reject_inline_secrets() {
        let input = VALID.replace(
            "postgresql://sift@warehouse.internal:5432/analytics?sslmode=verify-full",
            "postgresql://operator:secret@warehouse.internal:5432/analytics",
        );
        let error = Manifest::parse(&input).unwrap_err().to_string();
        assert!(error.contains("must not contain an inline password"));
        assert!(!error.contains("secret@"));

        assert!(validate_sql_server_connection_string(
            "connection",
            "Server=db.internal;Database=analytics;User ID=sift;Password=secret"
        )
        .is_err());
        assert!(validate_sql_server_connection_string(
            "connection",
            "Server=db.internal;Database=analytics;User ID=sift;Encrypt=true"
        )
        .is_ok());
    }

    #[test]
    fn semantic_edit_invalidates_lock() {
        let manifest = Manifest::parse(VALID).unwrap();
        let lock = LockFile::generate(&manifest, "0.1.0", 1).unwrap();
        let edited =
            Manifest::parse(&VALID.replace("allow_export = false", "allow_export = true")).unwrap();
        assert!(lock.verify(&edited).is_err());
    }

    #[test]
    fn runtime_must_match_locked_version_and_protocol() {
        let manifest = Manifest::parse(VALID).unwrap();
        let lock = LockFile::generate(&manifest, "0.1.0", 1).unwrap();
        lock.verify_runtime(&manifest, "0.1.0", 1).unwrap();
        assert!(lock.verify_runtime(&manifest, "0.1.1", 1).is_err());
        assert!(lock.verify_runtime(&manifest, "0.1.0", 2).is_err());
    }

    #[test]
    fn static_plan_contains_only_slot_identity() {
        let manifest = Manifest::parse(VALID).unwrap();
        let lock = LockFile::generate(&manifest, "0.1.0", 1).unwrap();
        let plan = manifest.static_plan(&lock).unwrap();
        assert_eq!(plan.resources.connections, 1);
        assert_eq!(plan.required_credentials.len(), 2);
        let encoded = serde_json::to_string(&plan).unwrap();
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("warehouse.internal"));
    }

    #[test]
    fn bundled_provider_configuration_matches_runtime_contract() {
        let manifest = Manifest::parse(VALID).unwrap();
        let postgres = manifest.connections[0].provider_configuration().unwrap();
        let spec = serde_json::from_value::<sift_protocol::ConnectionSpec>(postgres).unwrap();
        assert!(matches!(
            spec.engine_specific,
            Some(sift_protocol::EngineConnectionSpec::Postgres(_))
        ));

        let sql_server = sql_server_provider_configuration(
            "Server=db.internal,1433;Database=analytics;User ID=sift;Encrypt=true",
        )
        .unwrap();
        let spec = serde_json::from_value::<sift_protocol::ConnectionSpec>(sql_server).unwrap();
        assert!(matches!(
            spec.engine_specific,
            Some(sift_protocol::EngineConnectionSpec::SqlServer(_))
        ));
    }
}
