//! Selective recovery preserves destination identity and unrelated tenant state.
use super::*;

#[derive(Debug, Serialize)]
pub struct TenantRestoreReport {
    pub tenant_id: i64,
    pub applied: bool,
    pub source_instance_id: Option<String>,
    pub destination_instance_id: Option<String>,
    pub merge: sift_metadata::TenantMergeReport,
    pub rescue_archive: Option<PathBuf>,
}

fn audit(
    store: &MetadataStore,
    operation: &sift_protocol::Operation,
    status: &str,
) -> anyhow::Result<()> {
    let summary = operation.audit_summary();
    store.record_operation_audit(NewOperationAudit {
        actor_principal_id: None,
        action: if status == "started" {
            format!("{}.requested", summary.action)
        } else {
            summary.action
        },
        target: summary.target,
        target_id: summary.target_id,
        status: if status == "started" {
            "succeeded"
        } else {
            status
        }
        .into(),
        result_code: None,
        row_count: None,
        error_message: None,
        correlation_id: None,
    })?;
    Ok(())
}

pub async fn restore_tenant(
    config: &Config,
    archive: &Path,
    key_file: &Path,
    tenant_id: i64,
    apply: bool,
) -> anyhow::Result<TenantRestoreReport> {
    anyhow::ensure!(tenant_id > 0, "tenant ID must be positive");
    let _maintenance = crate::runtime::acquire_maintenance_exclusive(config)
        .context("tenant recovery requires the server to be stopped")?;
    if apply {
        recover_interrupted_restore(config)?;
    } else {
        anyhow::ensure!(
            read_restore_journal(config)?.is_none(),
            "finish interrupted recovery before previewing another restore"
        );
    }
    let metadata_path = configured_metadata_path(config)?;
    anyhow::ensure!(
        std::fs::symlink_metadata(&metadata_path)?.is_file(),
        "tenant recovery requires an existing regular metadata database"
    );
    let operation = sift_protocol::Operation::RestoreTenant {
        tenant_id,
        applied: apply,
    };
    {
        let store = MetadataStore::open(&metadata_path, Arc::new(MemorySecretStore::new()))?;
        store.ensure_schema_current()?;
        audit(&store, &operation, "started")?;
    }
    let result = prepare_and_restore(config, archive, key_file, tenant_id, apply, &operation).await;
    // Successful apply carries its completion audit in the atomically installed
    // snapshot. Preview/failure only adds audit; no tenant/auth/secret mutations.
    if (!apply || result.is_err()) && read_restore_journal(config)?.is_none() {
        let store = MetadataStore::open(&metadata_path, Arc::new(MemorySecretStore::new()))?;
        audit(
            &store,
            &operation,
            if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
        )?;
    }
    result
}

async fn prepare_and_restore(
    config: &Config,
    archive: &Path,
    key_file: &Path,
    tenant_id: i64,
    apply: bool,
    operation: &sift_protocol::Operation,
) -> anyhow::Result<TenantRestoreReport> {
    let metadata_path = configured_metadata_path(config)?;
    let parent = metadata_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = tempfile::Builder::new()
        .prefix(".sift-tenant-restore-")
        .tempdir_in(parent)?;
    let source_directory = directory.path().join("source");
    std::fs::create_dir(&source_directory)?;
    make_private_dir(&source_directory)?;
    let password = read_archive_password(key_file)?;
    let manifest = extract_archive(archive, &password, &source_directory)?;
    validate_restore_compatibility(config, &manifest, false)?;
    validate_staged_metadata(&source_directory.join(METADATA_ENTRY))?;
    let destination_instance_id = crate::runtime::existing_instance_id(config)?;
    anyhow::ensure!(manifest.source_instance_id==destination_instance_id,"tenant recovery requires the original installation identity; use full recovery for cross-instance migration");
    let working = directory.path().join("working.sqlite");
    {
        let store = MetadataStore::open(&metadata_path, Arc::new(MemorySecretStore::new()))?;
        store.ensure_schema_current()?;
        store.integrity_check()?;
        store.backup_database_to(&working)?;
    }
    let merge = sift_metadata::merge_tenant_snapshot(
        &working,
        &source_directory.join(METADATA_ENTRY),
        sift_metadata::TenantId(tenant_id),
    )?;
    let staged_secrets = match &manifest.secrets {
        SecretDisposition::File { portable: true } => {
            let key = configured_secret_key_path(config)?;
            let staged = directory.path().join("restored-secrets.enc");
            let original = secret_file_path(&metadata_path);
            if original.exists() {
                FileSecretStore::reencrypt(&original, &key, &staged, &key)?;
            } else {
                FileSecretStore::initialize_empty(&staged, &key)?;
            }
            let source = FileSecretStore::open(
                source_directory.join(SECRETS_ENTRY),
                source_directory.join(SOURCE_SECRET_KEY_ENTRY),
            )?;
            let destination = FileSecretStore::open(&staged, &key)?;
            for copy in &merge.secrets {
                let bytes = source
                    .get(&copy.namespace, &copy.source)
                    .await?
                    .context("selected tenant has a missing source secret")?;
                destination
                    .put(&copy.namespace, &copy.destination, &bytes)
                    .await?;
            }
            Some(staged)
        }
        SecretDisposition::Memory { .. } => {
            anyhow::ensure!(
                merge.secrets.is_empty(),
                "memory-secret backup cannot recover selected secret references"
            );
            None
        }
        _ => {
            bail!("tenant recovery requires portable file secrets or a secret-free memory snapshot")
        }
    };
    {
        let store = MetadataStore::open(&working, Arc::new(MemorySecretStore::new()))?;
        store.integrity_check()?;
        if apply {
            audit(&store, operation, "succeeded")?;
        }
        store.backup_database_to(&directory.path().join(METADATA_ENTRY))?;
    }
    let mut report = TenantRestoreReport {
        tenant_id,
        applied: apply,
        source_instance_id: manifest.source_instance_id,
        destination_instance_id,
        merge: merge.report,
        rescue_archive: None,
    };
    if !apply {
        return Ok(report);
    }
    let backup_dir = parent.join("backups");
    std::fs::create_dir_all(&backup_dir)?;
    make_private_dir(&backup_dir)?;
    let rescue = backup_dir.join(format!(
        "pre-tenant-restore-{}-{}.sift-backup",
        tenant_id,
        Uuid::new_v4()
    ));
    create_locked(config, &rescue, key_file, false)?;
    report.rescue_archive = Some(rescue);
    // From this point the durable journal owns staging; don't let TempDir remove
    // recovery inputs while a failed installation still needs them.
    let staging = directory.keep();
    let installed = (|| -> anyhow::Result<()> {
        install_staged_state(config, &staging, staged_secrets.as_deref())?;
        let store = crate::metadata_runtime::open_metadata_store(config)?
            .context("restored metadata is disabled")?;
        store.ensure_schema_current()?;
        store.integrity_check()?;
        commit_restore_journal(config)?;
        Ok(())
    })();
    if let Err(error) = installed {
        recover_interrupted_restore(config)
            .context("tenant install failed and automatic rollback failed")?;
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        return Err(error);
    }
    finalize_restore_journal(config)?;
    Ok(report)
}
