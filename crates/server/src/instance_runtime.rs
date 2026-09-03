//! Destination-private realization state for a two-file Sift instance root.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use base64::Engine as _;
use ed25519_dalek::VerifyingKey;
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sift_instance_config::{
    Deployment, LockFile, Manifest, RuntimeMode as InstanceRuntimeMode, SecretBackend, StaticPlan,
    Transport as InstanceTransport,
};

use crate::config::{Config, DeploymentPolicy, RuntimeMode, Transport};

pub const MANIFEST_FILE: &str = "sift.toml";
pub const LOCK_FILE: &str = "sift.lock";
const STATE_FORMAT_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const GENERATIONS_DIR: &str = "generations";
const CURRENT_FILE: &str = "current-generation.json";
const APPLY_LOCK_FILE: &str = "instance-apply.lock";

#[derive(Debug, Clone)]
pub struct InstanceRoot {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub lock: LockFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRecord {
    pub format_version: u32,
    pub generation: u64,
    pub manifest_id: uuid::Uuid,
    pub configuration_digest: String,
    pub lock_digest: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub resolved_bindings: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyReport {
    pub changed: bool,
    pub generation: u64,
    pub configuration_digest: String,
    pub lock_digest: String,
    pub state_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<sift_metadata::InstanceApplySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationView {
    pub current: bool,
    #[serde(flatten)]
    pub record: GenerationRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<GenerationApplyOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationApplyOutcome {
    pub status: String,
    pub at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl InstanceRoot {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let metadata = std::fs::symlink_metadata(root)
            .with_context(|| format!("reading server root metadata: {}", root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("server root must be a real directory, not a symlink");
        }
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("canonicalizing server root: {}", root.display()))?;
        let manifest = Manifest::parse(&read_bounded(&root.join(MANIFEST_FILE))?)
            .context("validating sift.toml")?;
        let lock =
            LockFile::parse(&read_bounded(&root.join(LOCK_FILE))?).context("parsing sift.lock")?;
        lock.verify_runtime(
            &manifest,
            crate::VERSION,
            sift_protocol::PROTOCOL_VERSION_NUMBER,
        )
        .context("verifying sift.lock against this Sift runtime")?;
        Ok(Self {
            root,
            manifest,
            lock,
        })
    }

    pub fn default_state_dir(&self) -> PathBuf {
        default_instance_state_root().join(self.manifest.manifest_id.to_string())
    }

    pub fn runtime_config(&self, state_dir: &Path) -> anyhow::Result<Config> {
        let deployment = match self.manifest.server.deployment {
            Deployment::Personal => DeploymentPolicy::Personal,
            Deployment::Team => DeploymentPolicy::Team,
        };
        let transport = match self.manifest.server.transport {
            InstanceTransport::Loopback => Transport::Loopback,
            InstanceTransport::Network => Transport::Network,
            InstanceTransport::SshProxy => Transport::SshProxy,
        };
        let mode = match self.manifest.server.mode {
            InstanceRuntimeMode::InProcess => RuntimeMode::InProcess,
            InstanceRuntimeMode::Daemon => RuntimeMode::Daemon,
            InstanceRuntimeMode::Container => RuntimeMode::Container,
        };
        let bind = match self.manifest.server.bind.as_str() {
            "auto-loopback" => "127.0.0.1:0".into(),
            "prompt-required" => bail!("server.bind is unresolved and requires operator input"),
            exact => exact.into(),
        };
        let mut config = Config {
            deployment,
            transport,
            mode,
            bind,
            ..Config::default()
        };
        config.runtime.state_dir = Some(state_dir.display().to_string());
        config.timeouts.request_secs = self.manifest.server.timeouts.request_secs;
        config.timeouts.shutdown_drain_secs = self.manifest.server.timeouts.shutdown_drain_secs;
        config.updater.enabled = self.manifest.server.updater.enabled;
        config.updater.channel = self.manifest.server.updater.channel.clone();
        config.updater.manifest_url = self.manifest.server.updater.manifest_url.clone();
        config.updater.signature_url = self.manifest.server.updater.signature_url.clone();
        config.updater.max_artifact_bytes = self.manifest.server.updater.max_artifact_bytes;
        config.updater.initial_delay_secs = self.manifest.server.updater.initial_delay_secs;
        config.updater.check_interval_secs = self.manifest.server.updater.check_interval_secs;
        config.updater.jitter_secs = self.manifest.server.updater.jitter_secs;
        config.log.filter = self.manifest.server.log.filter.clone();
        config.drivers.mock = self.manifest.server.drivers.mock;
        config.drivers.mock_extra = self.manifest.server.drivers.mock_extra;
        config.extensions.development_overrides = self
            .manifest
            .server
            .extension_policy
            .development_overrides
            .clone();
        config.extensions.allow_hosted_development = self
            .manifest
            .server
            .extension_policy
            .allow_hosted_development;
        config.metadata.enabled = true;
        config.metadata.path = Some(state_dir.join("metadata.sqlite3").display().to_string());
        config.metadata.secret_backend = match self.manifest.server.metadata.secret_backend {
            SecretBackend::File => "file",
            SecretBackend::Keychain => "keychain",
            SecretBackend::Memory => "memory",
        }
        .into();
        config.metadata.secret_key_file = (self.manifest.server.metadata.secret_backend
            == SecretBackend::File)
            .then(|| state_dir.join("secret.key").display().to_string());
        // The declarative bootstrap identity is reconciled before runtime
        // startup. Never create the legacy implicit local principal here.
        config.metadata.bootstrap_local = false;
        config.metadata.store_sql = self.manifest.server.metadata.store_sql;
        config.vault.max_label_bytes = usize::try_from(self.manifest.server.vault.max_label_bytes)
            .context("vault.max_label_bytes does not fit this platform")?;
        config.vault.max_metadata_bytes =
            usize::try_from(self.manifest.server.vault.max_metadata_bytes)
                .context("vault.max_metadata_bytes does not fit this platform")?;
        config.vault.max_secret_bytes =
            usize::try_from(self.manifest.server.vault.max_secret_bytes)
                .context("vault.max_secret_bytes does not fit this platform")?;
        config.vault.max_vaults_per_tenant = self.manifest.server.vault.max_vaults_per_tenant;
        config.vault.max_items_per_vault = self.manifest.server.vault.max_items_per_vault;
        config.vault.max_versions_per_item = self.manifest.server.vault.max_versions_per_item;
        config.vault.cleanup_batch_size = self.manifest.server.vault.cleanup_batch_size;
        config.vault.cleanup_interval_secs = self.manifest.server.vault.cleanup_interval_secs;
        config.vault.cleanup_retry_initial_secs =
            self.manifest.server.vault.cleanup_retry_initial_secs;
        config.vault.cleanup_retry_max_secs = self.manifest.server.vault.cleanup_retry_max_secs;
        config.audit.operation_log_path = self.manifest.server.audit.operation_log_path.clone();
        // Personal local-device instances deliberately use the OS account and
        // verified loopback peer as their zero-sign-in bootstrap guard. Team,
        // network, and hosted-code instances never receive this bypass.
        config.auth.loopback_bypass =
            self.manifest.auth.github.flow == sift_instance_config::GithubFlow::LocalDevice;
        config.auth.public_base_url = self.manifest.server.public_base_url.clone();
        config.limits.max_http_result_rows =
            usize::try_from(self.manifest.server.limits.max_http_result_rows)
                .context("max_http_result_rows does not fit this platform")?;
        config.limits.max_http_result_bytes =
            usize::try_from(self.manifest.server.limits.max_http_result_bytes)
                .context("max_http_result_bytes does not fit this platform")?;
        config.limits.max_cursors_per_session =
            usize::try_from(self.manifest.server.limits.max_cursors_per_session)
                .context("max_cursors_per_session does not fit this platform")?;
        config.limits.cursor_prefetch_pages =
            usize::try_from(self.manifest.server.limits.cursor_prefetch_pages)
                .context("cursor_prefetch_pages does not fit this platform")?;
        config.limits.cursor_spill_dir = self.manifest.server.limits.cursor_spill_dir.clone();
        config.limits.cursor_spill_ttl_secs = self.manifest.server.limits.cursor_spill_ttl_secs;
        config.limits.schema_cache_ttl_secs = self.manifest.server.limits.schema_cache_ttl_secs;
        config.limits.schema_mssql_poll_secs = self.manifest.server.limits.schema_mssql_poll_secs;
        config.limits.plan_capture_max_bytes =
            usize::try_from(self.manifest.server.limits.plan_capture_max_bytes)
                .context("plan_capture_max_bytes does not fit this platform")?;
        config.limits.plan_capture_max_per_tenant =
            self.manifest.server.limits.plan_capture_max_per_tenant;
        config.limits.plan_capture_max_per_source =
            self.manifest.server.limits.plan_capture_max_per_source;
        config.limits.plan_capture_max_age_days =
            self.manifest.server.limits.plan_capture_max_age_days;
        config.rate_limits.trusted_local_exempt =
            self.manifest.server.rate_limits.trusted_local_exempt;
        config.rate_limits.idle_ttl_secs = self.manifest.server.rate_limits.idle_ttl_secs;
        config.rate_limits.control = rate_bucket(self.manifest.server.rate_limits.control.as_ref());
        config.rate_limits.interactive =
            rate_bucket(self.manifest.server.rate_limits.interactive.as_ref());
        config.rate_limits.query = rate_bucket(self.manifest.server.rate_limits.query.as_ref());
        config.rate_limits.heavy_transfer =
            rate_bucket(self.manifest.server.rate_limits.heavy_transfer.as_ref());
        config.rate_limits.stream_bytes =
            rate_bucket(self.manifest.server.rate_limits.stream_bytes.as_ref());
        config.workspaces.enabled = self.manifest.server.workspaces.enabled;
        config.workspaces.roots = self
            .manifest
            .server
            .workspaces
            .roots
            .iter()
            .map(|root| crate::config::WorkspaceRootConfig {
                handle: root.handle.clone(),
                path: root.path.clone(),
                read_only: root.read_only,
            })
            .collect();
        config.vcs.enabled = self.manifest.server.vcs.enabled;
        config.vcs.network_enabled = self.manifest.server.vcs.network_enabled;
        config.vcs.executable = self.manifest.server.vcs.executable.clone();
        config.vcs.local_timeout_secs = self.manifest.server.vcs.local_timeout_secs;
        config.vcs.network_timeout_secs = self.manifest.server.vcs.network_timeout_secs;
        config.vcs.max_output_bytes = usize::try_from(self.manifest.server.vcs.max_output_bytes)
            .context("vcs.max_output_bytes does not fit this platform")?;
        config.vcs.max_file_bytes = usize::try_from(self.manifest.server.vcs.max_file_bytes)
            .context("vcs.max_file_bytes does not fit this platform")?;
        config.vcs.max_status_entries =
            usize::try_from(self.manifest.server.vcs.max_status_entries)
                .context("vcs.max_status_entries does not fit this platform")?;
        config.vcs.max_history_page = self.manifest.server.vcs.max_history_page;
        config.vcs.max_commit_files = usize::try_from(self.manifest.server.vcs.max_commit_files)
            .context("vcs.max_commit_files does not fit this platform")?;
        config.vcs.max_diff_files = usize::try_from(self.manifest.server.vcs.max_diff_files)
            .context("vcs.max_diff_files does not fit this platform")?;
        config.vcs.max_diff_hunks = usize::try_from(self.manifest.server.vcs.max_diff_hunks)
            .context("vcs.max_diff_hunks does not fit this platform")?;
        config.vcs.max_diff_lines = usize::try_from(self.manifest.server.vcs.max_diff_lines)
            .context("vcs.max_diff_lines does not fit this platform")?;
        config.tenant_limits.trusted_local_unlimited =
            self.manifest.server.tenant_limits.trusted_local_unlimited;
        config.tenant_limits.defaults =
            tenant_resource_limits(&self.manifest.server.tenant_limits.defaults);
        config.tenant_limits.ceilings =
            tenant_resource_limits(&self.manifest.server.tenant_limits.ceilings);
        // Format-v1 compatibility fields remain the final authority for these
        // two ceilings until a future format removes the duplication.
        config.tenant_limits.ceilings.connections =
            Some(u64::from(self.manifest.server.limits.max_connections));
        config.tenant_limits.ceilings.concurrent_queries = Some(u64::from(
            self.manifest.server.limits.max_concurrent_queries,
        ));
        config
            .validate()
            .context("validating realized server settings")?;
        Ok(config)
    }

    pub fn static_plan(&self) -> anyhow::Result<StaticPlan> {
        self.manifest.static_plan(&self.lock).map_err(Into::into)
    }

    pub fn generations(&self, state_dir: &Path) -> anyhow::Result<Vec<GenerationView>> {
        let current = read_current(state_dir)?;
        let generations = state_dir.join(GENERATIONS_DIR);
        let entries = match std::fs::read_dir(&generations) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading generation store: {}", generations.display())
                })
            }
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(number) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u64>().ok())
            else {
                continue;
            };
            let file_type = entry.file_type()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                bail!("generation {number} must be a regular directory");
            }
            let bytes = read_bounded_bytes(&entry.path().join("realization.json"), 64 * 1024)?;
            let record: GenerationRecord = serde_json::from_slice(&bytes)
                .with_context(|| format!("decoding generation {number}"))?;
            if record.format_version != STATE_FORMAT_VERSION
                || record.generation != number
                || record.manifest_id != self.manifest.manifest_id
            {
                bail!("generation {number} does not belong to this instance");
            }
            records.push(GenerationView {
                current: current
                    .as_ref()
                    .is_some_and(|selected| selected.generation == number),
                record,
                apply: read_generation_apply(&entry.path())?,
            });
        }
        records.sort_by_key(|view| view.record.generation);
        Ok(records)
    }

    pub fn generation_manifest(
        &self,
        state_dir: &Path,
        generation: u64,
    ) -> anyhow::Result<Manifest> {
        let path = state_dir
            .join(GENERATIONS_DIR)
            .join(generation.to_string())
            .join("normalized-manifest.json");
        let source = read_bounded(&path)?;
        serde_json::from_str(&source).context("parsing normalized generation manifest")
    }

    pub fn apply_generation(&self, state_dir: &Path) -> anyhow::Result<ApplyReport> {
        let (_lock_file, generations) = prepare_state(state_dir)?;

        let configuration_digest = self.manifest.configuration_digest()?;
        let lock_digest = self.lock.digest()?;
        if let Some(current) = read_current(state_dir)? {
            if current.configuration_digest == configuration_digest
                && current.lock_digest == lock_digest
            {
                return Ok(ApplyReport {
                    changed: false,
                    generation: current.generation,
                    configuration_digest,
                    lock_digest,
                    state_dir: state_dir.to_path_buf(),
                    metadata: None,
                });
            }
        }

        let record = self.stage_generation(state_dir, &generations)?;
        write_atomic_private(
            state_dir,
            CURRENT_FILE,
            &serde_json::to_vec_pretty(&record)?,
        )?;
        Ok(ApplyReport {
            changed: true,
            generation: record.generation,
            configuration_digest,
            lock_digest,
            state_dir: state_dir.to_path_buf(),
            metadata: None,
        })
    }

    /// Apply a complete instance while holding both the generation lock and
    /// the runtime maintenance lock. The generation pointer is switched only
    /// after metadata reconciliation commits.
    pub async fn apply(
        &self,
        state_dir: &Path,
        allow_destroy: bool,
    ) -> anyhow::Result<ApplyReport> {
        let (_apply_lock, generations) = prepare_state(state_dir)?;
        let configuration_digest = self.manifest.configuration_digest()?;
        let lock_digest = self.lock.digest()?;
        let current = read_current(state_dir)?;
        let unchanged = current.as_ref().is_some_and(|record| {
            record.manifest_id == self.manifest.manifest_id
                && record.configuration_digest == configuration_digest
                && record.lock_digest == lock_digest
        });
        let record = if unchanged {
            current.expect("checked current generation")
        } else {
            self.stage_generation(state_dir, &generations)?
        };

        let config = self.runtime_config(state_dir)?;
        ensure_file_secret_key(&config)?;
        let _maintenance = crate::runtime::acquire_maintenance_exclusive(&config)
            .context("a Sift runtime is using this destination; stop it before apply")?;
        let store = crate::metadata_runtime::open_metadata_store(&config)?
            .context("instance configuration unexpectedly disabled metadata")?;
        store
            .apply_migrations(false)
            .context("preparing instance metadata schema")?;
        let metadata = match store
            .apply_instance_manifest(&self.manifest, &self.lock, record.generation, allow_destroy)
            .await
        {
            Ok(metadata) => metadata,
            Err(error) => {
                record_generation_apply(
                    state_dir,
                    record.generation,
                    "failed",
                    Some(&error.to_string()),
                )?;
                return Err(error).context("reconciling manifest-managed resources");
            }
        };
        if let Err(error) = self.realize_extensions(state_dir, &store).await {
            record_generation_apply(
                state_dir,
                record.generation,
                "failed",
                Some(&error.to_string()),
            )?;
            return Err(error).context("realizing manifest-managed extensions");
        }
        record_generation_apply(state_dir, record.generation, "succeeded", None)?;

        if !unchanged {
            write_atomic_private(
                state_dir,
                CURRENT_FILE,
                &serde_json::to_vec_pretty(&record)?,
            )?;
        }
        Ok(ApplyReport {
            changed: !unchanged || metadata.changed,
            generation: record.generation,
            configuration_digest,
            lock_digest,
            state_dir: state_dir.to_path_buf(),
            metadata: Some(metadata),
        })
    }

    async fn realize_extensions(
        &self,
        state_dir: &Path,
        store: &sift_metadata::MetadataStore,
    ) -> anyhow::Result<()> {
        let package_limits = sift_plugin_host::PackageLimits::default();
        let registry = sift_plugin_host::ExtensionPackageRegistry::new(
            state_dir.join("extensions"),
            package_limits.clone(),
            store.clone(),
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .context("building extension artifact client")?;
        let desired = self
            .manifest
            .extensions
            .iter()
            .map(|extension| extension.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        for current in store.list_extension_selections()? {
            if !desired.contains(current.extension_id.as_str())
                && current.lifecycle != sift_protocol::ExtensionLifecycleState::Uninstalled
            {
                store.uninstall_extension(&current.extension_id, current.revision)?;
            }
        }

        for extension in &self.manifest.extensions {
            let (publisher, _) = extension
                .name
                .split_once('/')
                .context("validated extension id lost its publisher")?;
            let public_key_bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    extension
                        .publisher_public_key
                        .strip_prefix("base64:")
                        .context("validated publisher key lost its base64 prefix")?,
                )
                .context("decoding extension publisher key")?;
            let public_key_bytes: [u8; 32] = public_key_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("extension publisher key is not 32 bytes"))?;
            match store.extension_publisher_key(publisher, &extension.publisher_key) {
                Ok(existing) if existing.public_key == public_key_bytes => {}
                Ok(_) => bail!("trusted extension publisher key changed without a new fingerprint"),
                Err(sift_metadata::MetadataError::ExtensionNotFound(_)) => {
                    store.put_extension_publisher_key(
                        &sift_metadata::ExtensionPublisherKey {
                            publisher: publisher.into(),
                            fingerprint: extension.publisher_key.clone(),
                            public_key: public_key_bytes,
                            valid_from: chrono::Utc::now().to_rfc3339(),
                            valid_until: None,
                            revoked_at: None,
                            revision: 0,
                        },
                        None,
                    )?;
                }
                Err(error) => return Err(error.into()),
            }

            let archive_sha256 = extension
                .sha256
                .strip_prefix("sha256:")
                .expect("validated extension digest");
            let cached_marker = state_dir
                .join("extensions/packages")
                .join(archive_sha256)
                .join(".archive-sha256");
            let cached = std::fs::read_to_string(&cached_marker)
                .is_ok_and(|value| value.trim() == archive_sha256)
                && store
                    .selected_extension_package(&extension.name)
                    .is_ok_and(|package| {
                        package.selection.selected_archive_sha256 == archive_sha256
                            && package.version == extension.version
                    });
            if !cached {
                let mut response = client
                    .get(&extension.artifact)
                    .send()
                    .await
                    .context("downloading extension artifact")?
                    .error_for_status()
                    .context("extension artifact request failed")?;
                if response.url().scheme() != "https" {
                    bail!("extension artifact redirect left HTTPS");
                }
                if response
                    .content_length()
                    .is_some_and(|length| length > package_limits.max_archive_bytes)
                {
                    bail!("extension artifact exceeds configured package limit");
                }
                let temporary = tempfile::NamedTempFile::new_in(state_dir)
                    .context("creating extension artifact staging file")?;
                let mut file = tokio::fs::File::from_std(temporary.reopen()?);
                let mut length = 0_u64;
                let mut digest = Sha256::new();
                while let Some(chunk) = response.chunk().await? {
                    length = length
                        .checked_add(chunk.len() as u64)
                        .context("extension artifact length overflow")?;
                    if length > package_limits.max_archive_bytes {
                        bail!("extension artifact exceeds configured package limit");
                    }
                    digest.update(&chunk);
                    tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
                }
                tokio::io::AsyncWriteExt::flush(&mut file).await?;
                file.sync_all().await?;
                let observed = format!("sha256:{:x}", digest.finalize());
                if observed != extension.sha256 {
                    bail!("extension artifact SHA-256 does not match sift.lock");
                }

                let verifying_key = VerifyingKey::from_bytes(&public_key_bytes)
                    .context("extension publisher key is invalid")?;
                let installed = registry
                    .install(
                        temporary.path(),
                        sift_plugin_host::SignaturePolicy::RequireAny(std::slice::from_ref(
                            &verifying_key,
                        )),
                        sift_protocol::ExtensionProvenance::Verified,
                    )
                    .context("installing verified extension package")?;
                if installed.validated.manifest.id.to_string() != extension.name
                    || installed.validated.manifest.version != extension.version
                {
                    bail!("extension package identity or version does not match sift.lock");
                }
            }

            let current = store.extension_selection(&extension.name)?;
            let mut revision = current.revision;
            if current.selected_archive_sha256 != archive_sha256
                || !current.enabled
                || current.lifecycle != sift_protocol::ExtensionLifecycleState::Ready
            {
                revision = store
                    .update_extension_selection(sift_metadata::UpdateExtensionSelection {
                        extension_id: &extension.name,
                        selected_archive_sha256: Some(archive_sha256),
                        enabled: true,
                        lifecycle: sift_protocol::ExtensionLifecycleState::Ready,
                        isolation: current.isolation,
                        quarantine_reason: None,
                        expected_revision: current.revision,
                    })?
                    .revision;
            }
            if store.extension_grants(&extension.name)? != extension.grants {
                let grants = extension
                    .grants
                    .iter()
                    .map(|grant| {
                        serde_json::from_value::<sift_protocol::HostCapabilityKind>(
                            serde_json::Value::String(grant.clone()),
                        )
                        .map(|capability| (capability, "{}".into()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                store.replace_extension_grants(&sift_metadata::ReplaceExtensionGrants {
                    extension_id: extension.name.clone(),
                    grants,
                    expected_revision: revision,
                })?;
            }
        }
        Ok(())
    }

    fn stage_generation(
        &self,
        state_dir: &Path,
        generations: &Path,
    ) -> anyhow::Result<GenerationRecord> {
        let configuration_digest = self.manifest.configuration_digest()?;
        let lock_digest = self.lock.digest()?;
        let generation = next_generation(generations)?;
        let staging = generations.join(format!(".{generation}.staging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&staging)
            .with_context(|| format!("creating staged generation: {}", staging.display()))?;
        make_private_dir(&staging)?;
        let plan = self.static_plan()?;
        let mut bindings = std::collections::BTreeMap::new();
        let config = self.runtime_config(state_dir)?;
        bindings.insert("server.bind".into(), config.bind.clone());
        bindings.insert("runtime.state_dir".into(), state_dir.display().to_string());
        let record = GenerationRecord {
            format_version: STATE_FORMAT_VERSION,
            generation,
            manifest_id: self.manifest.manifest_id,
            configuration_digest: configuration_digest.clone(),
            lock_digest: lock_digest.clone(),
            created_at: chrono::Utc::now(),
            resolved_bindings: bindings,
        };
        write_private(
            &staging.join("normalized-manifest.json"),
            &serde_json::to_vec_pretty(&self.manifest)?,
        )?;
        write_private(
            &staging.join("lock.json"),
            &serde_json::to_vec_pretty(&self.lock)?,
        )?;
        write_private(
            &staging.join("plan.json"),
            &serde_json::to_vec_pretty(&plan)?,
        )?;
        write_private(
            &staging.join("realization.json"),
            &serde_json::to_vec_pretty(&record)?,
        )?;
        sync_dir(&staging)?;
        let final_dir = generations.join(generation.to_string());
        std::fs::rename(&staging, &final_dir)
            .with_context(|| format!("committing generation: {}", final_dir.display()))?;
        sync_dir(generations)?;
        Ok(record)
    }
}

fn rate_bucket(
    bucket: Option<&sift_instance_config::RateBucketConfig>,
) -> Option<crate::config::RateBucketConfig> {
    bucket.map(|bucket| crate::config::RateBucketConfig {
        refill_per_second: bucket.refill_per_second,
        burst: bucket.burst,
        cost: bucket.cost,
    })
}

fn tenant_resource_limits(
    limits: &sift_instance_config::TenantResourceLimits,
) -> sift_protocol::TenantResourceLimits {
    sift_protocol::TenantResourceLimits {
        connection_profiles: limits.connection_profiles,
        sessions: limits.sessions,
        connections: limits.connections,
        concurrent_queries: limits.concurrent_queries,
        cursors: limits.cursors,
        retained_result_bytes: limits.retained_result_bytes,
    }
}

fn prepare_state(state_dir: &Path) -> anyhow::Result<(File, PathBuf)> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating instance state: {}", state_dir.display()))?;
    make_private_dir(state_dir)?;
    let lock_file = private_open(&state_dir.join(APPLY_LOCK_FILE), true)?;
    lock_file
        .try_lock_exclusive()
        .context("another instance apply owns this destination")?;
    let generations = state_dir.join(GENERATIONS_DIR);
    std::fs::create_dir_all(&generations)
        .with_context(|| format!("creating generation store: {}", generations.display()))?;
    make_private_dir(&generations)?;
    Ok((lock_file, generations))
}

/// The single verified input to every instance-aware runtime path.
///
/// Construction proves that the source manifest and lock still match the
/// current immutable generation. Callers must not reconstruct `Config`
/// independently after this boundary.
pub struct AppliedInstance {
    pub root: InstanceRoot,
    pub generation: GenerationRecord,
    pub config: Config,
}

pub fn load_applied_instance(
    root: impl AsRef<Path>,
    state_dir: Option<&Path>,
) -> anyhow::Result<AppliedInstance> {
    let instance = InstanceRoot::open(root)?;
    let state_dir = state_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| instance.default_state_dir());
    let current = read_current(&state_dir)?.context(
        "instance has no applied generation; run `sift instance apply <server-root>` first",
    )?;
    if current.manifest_id != instance.manifest.manifest_id
        || current.configuration_digest != instance.manifest.configuration_digest()?
        || current.lock_digest != instance.lock.digest()?
    {
        bail!("server root differs from the applied generation; review and apply it first");
    }
    let config = instance.runtime_config(&state_dir)?;
    Ok(AppliedInstance {
        root: instance,
        generation: current,
        config,
    })
}

pub fn ensure_file_secret_key(config: &Config) -> anyhow::Result<()> {
    if config.metadata.secret_backend != "file" {
        return Ok(());
    }
    let path = Path::new(
        config
            .metadata
            .secret_key_file
            .as_deref()
            .context("file secret backend has no destination key path")?,
    );
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("secret key path must be a regular non-symlink file");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o077 != 0 {
                    bail!("secret key file must not be accessible by group or other users");
                }
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("checking destination secret key"),
    }
    let mut key = [0_u8; 32];
    getrandom::getrandom(&mut key)
        .map_err(|error| anyhow::anyhow!("generating destination secret key: {error}"))?;
    let mut encoded = String::with_capacity(65);
    for byte in key {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded.push('\n');
    write_private(path, encoded.as_bytes()).context("writing destination secret key")
}

fn default_instance_state_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(root).join("Sift").join("instances");
    }
    #[cfg(target_os = "macos")]
    if let Some(root) = std::env::var_os("HOME") {
        return PathBuf::from(root)
            .join("Library")
            .join("Application Support")
            .join("Sift")
            .join("instances");
    }
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(root).join("sift").join("instances");
    }
    if let Some(root) = std::env::var_os("HOME") {
        return PathBuf::from(root)
            .join(".local")
            .join("state")
            .join("sift")
            .join("instances");
    }
    std::env::temp_dir().join("sift").join("instances")
}

fn read_bounded(path: &Path) -> anyhow::Result<String> {
    String::from_utf8(read_bounded_bytes(path, MAX_CONFIG_BYTES)?)
        .with_context(|| format!("{} must be UTF-8", path.display()))
}

fn read_bounded_bytes(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading metadata: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular non-symlink file", path.display());
    }
    if metadata.len() > limit {
        bail!("{} exceeds the configuration byte limit", path.display());
    }
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > limit {
        bail!(
            "{} changed while reading or exceeds the byte limit",
            path.display()
        );
    }
    Ok(bytes)
}

fn read_current(state_dir: &Path) -> anyhow::Result<Option<GenerationRecord>> {
    let path = state_dir.join(CURRENT_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("reading metadata for {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("current generation pointer must be a regular non-symlink file");
    }
    if metadata.len() > 64 * 1024 {
        bail!("current generation record exceeds the byte limit");
    }
    let mut source = Vec::with_capacity(metadata.len() as usize);
    File::open(&path)
        .with_context(|| format!("opening {}", path.display()))?
        .take(64 * 1024 + 1)
        .read_to_end(&mut source)
        .with_context(|| format!("reading {}", path.display()))?;
    if source.len() > 64 * 1024 {
        bail!("current generation pointer changed while reading");
    }
    let record: GenerationRecord =
        serde_json::from_slice(&source).context("decoding current generation record")?;
    if record.format_version != STATE_FORMAT_VERSION {
        bail!("unsupported private generation format");
    }
    Ok(Some(record))
}

fn read_generation_apply(directory: &Path) -> anyhow::Result<Option<GenerationApplyOutcome>> {
    let path = directory.join("apply-report.json");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => serde_json::from_slice(&read_bounded_bytes(&path, 16 * 1024)?)
            .map(Some)
            .context("decoding generation apply report"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("reading generation apply report"),
    }
}

fn record_generation_apply(
    state_dir: &Path,
    generation: u64,
    status: &str,
    message: Option<&str>,
) -> anyhow::Result<()> {
    let message = message.map(|message| message.chars().take(2_048).collect());
    let report = GenerationApplyOutcome {
        status: status.into(),
        at: chrono::Utc::now(),
        message,
    };
    write_atomic_private(
        &state_dir.join(GENERATIONS_DIR).join(generation.to_string()),
        "apply-report.json",
        &serde_json::to_vec_pretty(&report)?,
    )
}

fn next_generation(generations: &Path) -> anyhow::Result<u64> {
    let mut maximum = 0_u64;
    for entry in std::fs::read_dir(generations)
        .with_context(|| format!("reading generation store: {}", generations.display()))?
    {
        let entry = entry?;
        if let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        {
            maximum = maximum.max(number);
        }
    }
    maximum
        .checked_add(1)
        .context("generation counter overflow")
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        make_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    private_mode(&mut options);
    let mut file = options
        .open(path)
        .with_context(|| format!("creating private file: {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_atomic_private(parent: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let staging_name = format!(".{name}.{}", uuid::Uuid::new_v4());
    let staging = parent.join(staging_name);
    write_private(&staging, bytes)?;
    let destination = parent.join(name);
    #[cfg(windows)]
    if destination.exists() {
        let backup = parent.join(format!(".{name}.previous"));
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(&destination, &backup)?;
        if let Err(error) = std::fs::rename(&staging, &destination) {
            let _ = std::fs::rename(&backup, &destination);
            return Err(error).context("replacing private generation pointer");
        }
        let _ = std::fs::remove_file(backup);
    }
    #[cfg(not(windows))]
    std::fs::rename(&staging, &destination)?;
    sync_dir(parent)
}

fn private_open(path: &Path, create: bool) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    private_mode(&mut options);
    options
        .open(path)
        .with_context(|| format!("opening private file: {}", path.display()))
}

#[cfg(unix)]
fn private_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_mode(_options: &mut OpenOptions) {}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_demo(root: &Path) {
        std::fs::create_dir(root).unwrap();
        let demo =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reproducible-instance");
        std::fs::copy(demo.join(MANIFEST_FILE), root.join(MANIFEST_FILE)).unwrap();
        std::fs::copy(demo.join(LOCK_FILE), root.join(LOCK_FILE)).unwrap();
    }

    #[test]
    fn apply_is_immutable_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let state = directory.path().join("state");
        copy_demo(&root);
        let instance = InstanceRoot::open(&root).unwrap();
        let first = instance.apply_generation(&state).unwrap();
        let second = instance.apply_generation(&state).unwrap();
        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(first.generation, second.generation);
        let applied = load_applied_instance(&root, Some(&state)).unwrap();
        assert_eq!(applied.generation.generation, first.generation);
        assert_eq!(applied.root.manifest, instance.manifest);
        assert_eq!(
            applied.config.runtime.state_dir.as_deref(),
            Some(state.to_str().unwrap())
        );
        assert!(state.join("generations/1/realization.json").is_file());
        let generations = instance.generations(&state).unwrap();
        assert_eq!(generations.len(), 1);
        assert!(generations[0].current);
        assert_eq!(
            instance.generation_manifest(&state, 1).unwrap(),
            instance.manifest
        );
        record_generation_apply(&state, 1, "failed", Some(&"x".repeat(3_000))).unwrap();
        let apply = read_generation_apply(&state.join("generations/1"))
            .unwrap()
            .unwrap();
        assert_eq!(apply.status, "failed");
        assert_eq!(apply.message.unwrap().chars().count(), 2_048);
    }

    #[test]
    fn unapplied_edit_cannot_start() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let state = directory.path().join("state");
        copy_demo(&root);
        let instance = InstanceRoot::open(&root).unwrap();
        instance.apply_generation(&state).unwrap();
        let mut manifest = instance.manifest.clone();
        manifest.name = "edited-sift".into();
        std::fs::write(root.join(MANIFEST_FILE), manifest.to_toml_pretty().unwrap()).unwrap();
        assert!(load_applied_instance(&root, Some(&state)).is_err());
    }

    #[test]
    fn runtime_config_realizes_workspace_and_vcs_settings() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let state = directory.path().join("state");
        let workspace = directory.path().join("workspace");
        copy_demo(&root);
        std::fs::create_dir(&workspace).unwrap();
        let mut instance = InstanceRoot::open(&root).unwrap();
        instance.manifest.server.workspaces.enabled = true;
        instance.manifest.server.workspaces.roots =
            vec![sift_instance_config::WorkspaceRootConfig {
                handle: "demo-postgres".into(),
                path: workspace.display().to_string(),
                read_only: false,
            }];
        instance.manifest.server.vcs.enabled = true;

        let config = instance.runtime_config(&state).unwrap();
        assert!(config.workspaces.enabled);
        assert_eq!(config.workspaces.roots[0].handle, "demo-postgres");
        assert_eq!(
            config.workspaces.roots[0].path,
            workspace.display().to_string()
        );
        assert!(config.vcs.enabled);
        assert!(!config.vcs.network_enabled);
    }
}
