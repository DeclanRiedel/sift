//! Offline, timer-friendly backup policy with a durable upload checkpoint.
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub id: Uuid,
    pub directory: PathBuf,
    pub key_file: PathBuf,
    pub interval_seconds: u64,
    pub keep_last: usize,
    /// Private file containing a complete HTTPS PUT URL, typically presigned.
    /// The operator supplies a fresh unique destination for each occurrence.
    pub remote_url_file: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    completed: Vec<Entry>,
    pending: Option<Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: Uuid,
    created_at: DateTime<Utc>,
}

impl Entry {
    fn path(&self, directory: &Path) -> PathBuf {
        directory.join(format!("{}.sift-backup", self.id))
    }
}

#[derive(Serialize)]
pub struct Report {
    due: bool,
    uploaded: bool,
    retained: usize,
    pruned: usize,
}

pub async fn run(config: &Config, policy_path: &Path) -> anyhow::Result<Report> {
    let file = File::open(policy_path).context("reading backup policy")?;
    anyhow::ensure!(
        file.metadata()?.len() <= 64 * 1024,
        "backup policy exceeds 64 KiB"
    );
    let policy: Policy =
        serde_json::from_reader(file.take(64 * 1024 + 1)).context("invalid backup policy")?;
    let store = crate::metadata_runtime::open_metadata_store(config)?
        .context("backup policy requires metadata")?;
    record(&store, false, 0, "started")?;
    let result = run_at(config, policy, Utc::now()).await;
    if result.is_err() {
        record(&store, false, 0, "failed")?;
    }
    if result.as_ref().is_ok_and(|report| !report.due) {
        record(&store, false, 0, "succeeded")?;
    }
    result
}

async fn run_at(config: &Config, policy: Policy, now: DateTime<Utc>) -> anyhow::Result<Report> {
    anyhow::ensure!(
        (60..=31_536_000).contains(&policy.interval_seconds)
            && (1..=1000).contains(&policy.keep_last),
        "backup interval must be 60..31536000 seconds and keep_last 1..1000"
    );
    anyhow::ensure!(
        policy.directory.is_absolute() && policy.key_file.is_absolute(),
        "backup directory and key file must be absolute paths"
    );
    let _maintenance = crate::runtime::acquire_maintenance_exclusive(config)
        .context("backup policies require a stopped server")?;
    let directory = policy.directory.join(policy.id.to_string());
    std::fs::create_dir_all(&policy.directory)?;
    if !directory.exists() {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
    }
    anyhow::ensure!(
        std::fs::symlink_metadata(&directory)?.file_type().is_dir(),
        "backup policy directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            std::fs::metadata(&directory)?.permissions().mode() & 0o077 == 0,
            "backup policy directory must be private"
        );
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(directory.join("policy.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("backup policy is already running")?;
    let ledger_path = directory.join("ledger.json");
    let mut ledger: Ledger = if ledger_path.exists() {
        let file = File::open(&ledger_path)?;
        anyhow::ensure!(
            file.metadata()?.len() <= 1024 * 1024,
            "backup ledger exceeds limit"
        );
        serde_json::from_reader(file).context("invalid backup ledger")?
    } else {
        Ledger::default()
    };
    if ledger.pending.is_none()
        && ledger.completed.last().is_some_and(|entry| {
            now.signed_duration_since(entry.created_at).num_seconds()
                < policy.interval_seconds as i64
        })
    {
        return Ok(Report {
            due: false,
            uploaded: false,
            retained: ledger.completed.len(),
            pruned: 0,
        });
    }
    recover_interrupted_restore(config)?;
    if ledger.pending.is_none() {
        ledger.pending = Some(Entry {
            id: Uuid::new_v4(),
            created_at: now,
        });
        save(&ledger_path, &ledger)?;
    }
    let entry = ledger.pending.clone().context("missing pending backup")?;
    let archive = entry.path(&directory);
    if !archive.exists() {
        create_locked(config, &archive, &policy.key_file, true)?;
    }
    // A recovered pending archive must authenticate before upload or retention.
    inspect(&archive, &policy.key_file)?;
    let uploaded = if let Some(url_file) = &policy.remote_url_file {
        upload(&archive, url_file).await?;
        true
    } else {
        false
    };
    ledger.completed.push(entry);
    ledger.pending = None;
    save(&ledger_path, &ledger)?;
    let mut pruned = 0;
    while ledger.completed.len() > policy.keep_last {
        let path = ledger.completed[0].path(&directory);
        // Only exact UUID entries owned by this policy ledger are ever removed.
        if path.exists() {
            anyhow::ensure!(
                std::fs::symlink_metadata(&path)?.file_type().is_file(),
                "retention target must be a regular archive"
            );
            std::fs::remove_file(path)?;
            sync_dir(&directory)?;
        }
        ledger.completed.remove(0);
        save(&ledger_path, &ledger)?;
        pruned += 1;
    }
    let store = crate::metadata_runtime::open_metadata_store(config)?
        .context("backup policy requires metadata")?;
    record(&store, uploaded, pruned, "succeeded")?;
    Ok(Report {
        due: true,
        uploaded,
        retained: ledger.completed.len(),
        pruned,
    })
}

fn record(
    store: &MetadataStore,
    uploaded: bool,
    pruned: usize,
    status: &str,
) -> anyhow::Result<()> {
    let operation = sift_protocol::Operation::BackupPolicy {
        uploaded,
        pruned: pruned as u64,
    };
    let summary = operation.audit_summary();
    store.record_operation_audit(NewOperationAudit {
        actor_principal_id: None,
        action: if status == "started" {
            format!("{}.requested", summary.action)
        } else {
            summary.action
        },
        target: summary.target,
        target_id: None,
        status: if status == "started" {
            "succeeded"
        } else {
            status
        }
        .into(),
        result_code: None,
        row_count: Some(pruned as i64),
        error_message: None,
        correlation_id: None,
    })?;
    Ok(())
}

fn save(path: &Path, ledger: &Ledger) -> anyhow::Result<()> {
    let parent = path.parent().context("ledger parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temporary.as_file_mut(), ledger)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path)?;
    sync_dir(parent)?;
    Ok(())
}

async fn upload(archive: &Path, url_file: &Path) -> anyhow::Result<()> {
    let metadata =
        std::fs::symlink_metadata(url_file).context("reading private upload URL file")?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024,
        "upload URL must be a bounded regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "upload URL file must be private"
        );
    }
    let raw =
        zeroize::Zeroizing::new(std::fs::read_to_string(url_file).context("reading upload URL")?);
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid archive filename")?;
    let expanded = zeroize::Zeroizing::new(raw.trim().replace("{archive}", name));
    let url = url::Url::parse(&expanded).map_err(|_| anyhow::anyhow!("invalid upload URL"))?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "upload requires HTTPS without userinfo or fragment"
    );
    put_archive(archive, url).await
}

async fn put_archive(archive: &Path, url: url::Url) -> anyhow::Result<()> {
    let file = tokio::fs::File::open(archive).await?;
    let length = file.metadata().await?.len();
    let stream = tokio_util::io::ReaderStream::new(file);
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(300))
        .build()?
        .put(url)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .header(reqwest::header::CONTENT_LENGTH, length)
        .header(reqwest::header::IF_NONE_MATCH, "*")
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("backup upload transport failed; local archive retained"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "backup upload rejected; local archive retained"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_put_streams_bytes_and_requires_no_overwrite() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let app = axum::Router::new().route(
            "/archive",
            axum::routing::put(move |headers: axum::http::HeaderMap, body: bytes::Bytes| {
                let tx = tx.clone();
                async move {
                    assert_eq!(headers[reqwest::header::IF_NONE_MATCH], "*");
                    tx.send(body).await.unwrap();
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut archive = tempfile::NamedTempFile::new().unwrap();
        archive.write_all(b"encrypted archive payload").unwrap();
        let url = url::Url::parse(&format!("http://{address}/archive")).unwrap();
        put_archive(archive.path(), url.clone()).await.unwrap();
        assert_eq!(&rx.recv().await.unwrap()[..], b"encrypted archive payload");
        assert!(put_archive(archive.path(), url.join("/missing").unwrap())
            .await
            .is_err());
        assert!(archive.path().exists());
        task.abort();
    }

    #[tokio::test]
    async fn schedule_retention_and_failed_upload_keep_recovery_copies() {
        let root = tempfile::tempdir().unwrap();
        let config = super::super::tests::memory_config(root.path());
        let store = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("backup policy test").unwrap();
        let key = root.path().join("archive.key");
        super::super::tests::write_private_key(&key, "a");
        let id = Uuid::new_v4();
        let make_policy = || Policy {
            id,
            directory: root.path().join("backups"),
            key_file: key.clone(),
            interval_seconds: 60,
            keep_last: 1,
            remote_url_file: None,
        };
        let mut policy_file = tempfile::NamedTempFile::new().unwrap();
        serde_json::to_writer(policy_file.as_file_mut(), &make_policy()).unwrap();
        assert!(run(&config, policy_file.path()).await.unwrap().due);
        let now = Utc::now();
        let unrelated = make_policy()
            .directory
            .join(id.to_string())
            .join(format!("{}.sift-backup", Uuid::new_v4()));
        std::fs::write(&unrelated, b"not owned by this policy").unwrap();
        assert!(!run_at(&config, make_policy(), now).await.unwrap().due);
        assert_eq!(
            run_at(&config, make_policy(), now + chrono::Duration::seconds(61))
                .await
                .unwrap()
                .pruned,
            1
        );
        assert_eq!(
            std::fs::read(&unrelated).unwrap(),
            b"not owned by this policy"
        );
        let mut failure = make_policy();
        failure.remote_url_file = Some(root.path().join("missing-url"));
        assert!(
            run_at(&config, failure, now + chrono::Duration::seconds(122))
                .await
                .is_err()
        );
        let ledger: Ledger = serde_json::from_reader(
            File::open(
                make_policy()
                    .directory
                    .join(id.to_string())
                    .join("ledger.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(ledger.pending.is_some());
        assert_eq!(ledger.completed.len(), 1);
        let directory = make_policy().directory.join(id.to_string());
        assert!(ledger.completed[0].path(&directory).exists());
        assert!(ledger.pending.unwrap().path(&directory).exists());
        // Reuse the authenticated pending archive after fixing the destination.
        assert_eq!(
            run_at(&config, make_policy(), now + chrono::Duration::seconds(123))
                .await
                .unwrap()
                .pruned,
            1
        );
    }
}
