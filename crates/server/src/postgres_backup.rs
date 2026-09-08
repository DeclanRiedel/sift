//! Operator-only PostgreSQL custom archives using explicitly selected tools.
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub tools_directory: PathBuf,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password_file: Option<PathBuf>,
    #[serde(default = "default_ssl")]
    pub ssl_mode: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_ssl() -> String {
    "verify-full".into()
}
fn default_timeout() -> u64 {
    3600
}

impl Spec {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.tools_directory.is_absolute(),
            "PostgreSQL tools directory must be absolute"
        );
        ensure!(
            self.port > 0 && (1..=86400).contains(&self.timeout_seconds),
            "invalid PostgreSQL port or timeout"
        );
        for value in [&self.host, &self.database, &self.user] {
            ensure!(
                !value.is_empty()
                    && value.len() <= 1024
                    && !value.contains(['\0', '\n', '\r', '='])
                    && !value.contains("://"),
                "PostgreSQL target must use explicit fields, not a connection string"
            );
        }
        ensure!(
            matches!(
                self.ssl_mode.as_str(),
                "disable" | "require" | "verify-ca" | "verify-full"
            ),
            "invalid PostgreSQL ssl_mode"
        );
        if let Some(path) = &self.password_file {
            ensure!(
                path.is_absolute(),
                "PostgreSQL password file must be absolute"
            );
            let metadata = std::fs::symlink_metadata(path)
                .context("reading PostgreSQL password file metadata")?;
            ensure!(
                metadata.is_file(),
                "PostgreSQL password file must be a regular file"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    metadata.permissions().mode() & 0o077 == 0,
                    "PostgreSQL password file must be private"
                );
            }
        }
        Ok(())
    }

    fn command(&self, name: &str) -> Command {
        let executable = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        let mut command = Command::new(self.tools_directory.join(executable));
        // No inherited libpq service, password, options, host, or database.
        command
            .env_clear()
            .env("PGHOST", &self.host)
            .env("PGPORT", self.port.to_string())
            .env("PGDATABASE", &self.database)
            .env("PGUSER", &self.user)
            .env("PGSSLMODE", &self.ssl_mode)
            .env("PGCONNECT_TIMEOUT", "15")
            .env("PGAPPNAME", "sift-backup")
            .env("PGCLIENTENCODING", "UTF8")
            .env(
                "PGPASSFILE",
                self.password_file
                    .as_deref()
                    .unwrap_or(Path::new(if cfg!(windows) { "NUL" } else { "/dev/null" })),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }
}

#[derive(Serialize)]
pub struct Report {
    pub action: &'static str,
    pub applied: bool,
    pub archive_bytes: u64,
}

pub async fn run(
    config: &crate::config::Config,
    spec_path: &Path,
    archive: &Path,
    restore: bool,
    apply: bool,
) -> anyhow::Result<Report> {
    let file = File::open(spec_path).context("opening PostgreSQL backup specification")?;
    ensure!(
        file.metadata()?.len() <= 64 * 1024,
        "PostgreSQL specification exceeds 64 KiB"
    );
    let spec: Spec =
        serde_json::from_reader(file).context("invalid PostgreSQL backup specification")?;
    spec.validate()?;
    let store = crate::metadata_runtime::open_metadata_store(config)?
        .context("PostgreSQL backup requires metadata for audit")?;
    let operation = if restore {
        sift_protocol::Operation::RestorePostgres { applied: apply }
    } else {
        sift_protocol::Operation::DumpPostgres
    };
    record(&store, &operation, "started")?;
    let result = if restore {
        restore_archive(&spec, archive, apply).await
    } else {
        dump(&spec, archive).await
    };
    record(
        &store,
        &operation,
        if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        },
    )?;
    result
}

fn record(
    store: &sift_metadata::MetadataStore,
    operation: &sift_protocol::Operation,
    status: &str,
) -> anyhow::Result<()> {
    let summary = operation.audit_summary();
    store.record_operation_audit(sift_metadata::NewOperationAudit {
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
        row_count: None,
        error_message: None,
        correlation_id: None,
    })?;
    Ok(())
}

async fn execute(command: &mut Command, timeout: Duration) -> anyhow::Result<()> {
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("could not start configured PostgreSQL tool"))?;
    let result = tokio::select! {
        result = child.wait() => Some(result),
        _ = tokio::time::sleep(timeout) => None,
        _ = tokio::signal::ctrl_c() => None,
    };
    match result {
        Some(status) => ensure!(
            status.context("waiting for PostgreSQL tool")?.success(),
            "PostgreSQL tool failed; raw diagnostics suppressed to protect credentials and data"
        ),
        None => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("PostgreSQL tool timed out or was cancelled");
        }
    }
    Ok(())
}

async fn dump(spec: &Spec, output: &Path) -> anyhow::Result<Report> {
    ensure!(!output.exists(), "PostgreSQL dump output already exists");
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut command = spec.command("pg_dump");
    command
        .args(["--format=custom", "--no-password", "--no-acl"])
        .stdout(temporary.as_file().try_clone()?);
    execute(&mut command, Duration::from_secs(spec.timeout_seconds)).await?;
    temporary.as_file().sync_all()?;
    let bytes = temporary.as_file().metadata()?.len();
    ensure!(bytes > 5, "PostgreSQL dump was empty");
    temporary
        .persist_noclobber(output)
        .map_err(|_| anyhow::anyhow!("could not publish PostgreSQL archive without overwriting"))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(Report {
        action: "dump",
        applied: true,
        archive_bytes: bytes,
    })
}

async fn restore_archive(spec: &Spec, archive: &Path, apply: bool) -> anyhow::Result<Report> {
    ensure!(
        std::fs::symlink_metadata(archive)?.is_file(),
        "PostgreSQL archive must be a regular file"
    );
    // Snapshot the selected archive privately so validation and apply consume
    // the same bytes even if an external process replaces the original path.
    let mut source = File::open(archive)?;
    ensure!(
        source.metadata()?.len() <= 16 * 1024 * 1024 * 1024,
        "PostgreSQL restore archive exceeds 16 GiB"
    );
    let mut magic = [0; 5];
    source.read_exact(&mut magic)?;
    ensure!(
        &magic == b"PGDMP",
        "only PostgreSQL custom archives are supported"
    );
    use std::io::{Seek, SeekFrom};
    source.seek(SeekFrom::Start(0))?;
    let mut snapshot = tempfile::NamedTempFile::new()?;
    let bytes = std::io::copy(&mut source, snapshot.as_file_mut())?;
    snapshot.as_file().sync_all()?;
    let mut validate = spec.command("pg_restore");
    validate.arg("--file=-").arg(snapshot.path());
    execute(&mut validate, Duration::from_secs(spec.timeout_seconds)).await?;
    if apply {
        let mut command = spec.command("pg_restore");
        command
            .args([
                "--single-transaction",
                "--exit-on-error",
                "--no-owner",
                "--no-acl",
                "--no-password",
                "--dbname",
            ])
            .arg(&spec.database)
            .arg(snapshot.path());
        execute(&mut command, Duration::from_secs(spec.timeout_seconds)).await?;
    }
    Ok(Report {
        action: "restore",
        applied: apply,
        archive_bytes: bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, feature = "live-pg"))]
    #[tokio::test]
    async fn real_postgres_dump_dry_run_restore_and_rollback() {
        let tools = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .find(|path| path.join("pg_dump").is_file() && path.join("initdb").is_file())
            .expect("live-pg backup tests require PostgreSQL tools on PATH");
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let status = std::process::Command::new(tools.join("initdb"))
            .args(["--auth=trust", "--username=sift_test", "--no-locale", "-D"])
            .arg(&data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        struct Server(std::process::Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _server = Server(
            std::process::Command::new(tools.join("postgres"))
                .arg("-D")
                .arg(&data)
                .arg("-k")
                .arg(root.path())
                .args(["-c", "listen_addresses=", "-p", "5432"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut spec = Spec {
            tools_directory: tools,
            host: root.path().display().to_string(),
            port: 5432,
            database: "postgres".into(),
            user: "sift_test".into(),
            password_file: None,
            ssl_mode: "disable".into(),
            timeout_seconds: 10,
        };
        let sql = |spec: &Spec, sql: &str| {
            let mut command = spec.command("psql");
            command.args(["--no-password", "-v", "ON_ERROR_STOP=1", "-c", sql]);
            command
        };
        let mut ready = false;
        for _ in 0..100 {
            if execute(&mut sql(&spec, "SELECT 1"), Duration::from_secs(1))
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ready);
        let started = std::time::Instant::now();
        assert!(execute(
            &mut sql(&spec, "SELECT pg_sleep(10)"),
            Duration::from_millis(30)
        )
        .await
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        execute(
            &mut sql(&spec, "CREATE DATABASE source"),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        execute(
            &mut sql(&spec, "CREATE DATABASE destination"),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        spec.database = "source".into();
        execute(&mut sql(&spec, "CREATE TABLE sample(id bigint PRIMARY KEY, label text); INSERT INTO sample VALUES(9223372036854775807,'restored')"), Duration::from_secs(5)).await.unwrap();
        let archive = root.path().join("source.dump");
        let config = crate::config::Config {
            metadata: crate::config::MetadataConfig {
                path: Some(root.path().join("metadata.sqlite").display().to_string()),
                secret_backend: "memory".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let metadata = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        metadata.apply_migrations(false).unwrap();
        let mut spec_file = tempfile::NamedTempFile::new().unwrap();
        serde_json::to_writer(spec_file.as_file_mut(), &spec).unwrap();
        assert!(
            run(&config, spec_file.path(), &archive, false, false)
                .await
                .unwrap()
                .archive_bytes
                > 5
        );
        assert!(dump(&spec, &archive).await.is_err());
        spec.database = "destination".into();
        assert!(
            !restore_archive(&spec, &archive, false)
                .await
                .unwrap()
                .applied
        );
        execute(&mut sql(&spec, "DO $$ BEGIN IF to_regclass('sample') IS NOT NULL THEN RAISE EXCEPTION 'dry run mutated'; END IF; END $$"), Duration::from_secs(5)).await.unwrap();
        assert!(
            restore_archive(&spec, &archive, true)
                .await
                .unwrap()
                .applied
        );
        execute(&mut sql(&spec, "DO $$ BEGIN IF (SELECT label FROM sample WHERE id=9223372036854775807) IS DISTINCT FROM 'restored' THEN RAISE EXCEPTION 'bad restore'; END IF; END $$"), Duration::from_secs(5)).await.unwrap();
        assert!(restore_archive(&spec, &archive, true).await.is_err());
        execute(&mut sql(&spec, "DO $$ BEGIN IF (SELECT count(*) FROM sample) <> 1 THEN RAISE EXCEPTION 'failed restore changed rows'; END IF; END $$"), Duration::from_secs(5)).await.unwrap();
    }
    #[test]
    fn rejects_connection_string_targets_and_inherited_passwords() {
        let mut spec = Spec {
            tools_directory: PathBuf::from(if cfg!(windows) { "C:\\tools" } else { "/tools" }),
            host: "localhost".into(),
            port: 5432,
            database: "target".into(),
            user: "owner".into(),
            password_file: None,
            ssl_mode: default_ssl(),
            timeout_seconds: 60,
        };
        spec.validate().unwrap();
        let command = spec.command("pg_restore");
        let env = command.as_std().get_envs().collect::<Vec<_>>();
        assert!(!env.iter().any(|(key, _)| *key == "PGPASSWORD"));
        assert!(env.iter().any(|(key, _)| *key == "PGPASSFILE"));
        spec.database = "dbname=other host=attacker".into();
        assert!(spec.validate().is_err());
    }
}
