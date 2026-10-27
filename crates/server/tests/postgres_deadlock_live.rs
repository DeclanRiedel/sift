//! Opt-in native JSON log acceptance in a disposable, socket-only cluster.
#![cfg(all(unix, feature = "live-pg"))]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;

use sift_instance_config::PostgresDeadlockLogSource;
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::{Engine, OpenSessionRequest};
use sift_server::{DriverRegistry, SessionStore};

struct Cluster(tempfile::TempDir);

impl Cluster {
    fn command(&self, program: &str, args: &[&str]) -> Output {
        let output = Command::new(program)
            .args(args)
            .output()
            .expect("PostgreSQL dev tools must be installed");
        assert!(
            output.status.success(),
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn psql(&self) -> Command {
        let mut command = Command::new("psql");
        command.args([
            "-X",
            "-A",
            "-t",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            self.0.path().to_str().unwrap(),
            "-U",
            "sift",
            "-d",
            "postgres",
        ]);
        command
    }

    fn sql(&self, sql: &str) -> String {
        let output = self.psql().args(["-c", sql]).output().unwrap();
        assert!(
            output.status.success(),
            "fixture SQL failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn locked_row(&self, id: u32) -> Child {
        let mut child = self
            .psql()
            .arg("-q")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        writeln!(child.stdin.as_mut().unwrap(), "SET statement_timeout='5s'; BEGIN; UPDATE deadlock_rows SET value=1 WHERE id={id}; SELECT 'ready';").unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        assert_eq!(line.trim(), "ready");
        child
    }

    fn start() -> Self {
        let cluster = Self(tempfile::tempdir().unwrap());
        let data = cluster.0.path().join("data");
        cluster.command(
            "initdb",
            &[
                "-D",
                data.to_str().unwrap(),
                "-U",
                "sift",
                "--auth=trust",
                "--no-locale",
                "--encoding=UTF8",
            ],
        );
        let socket = cluster
            .0
            .path()
            .to_str()
            .unwrap()
            .replace('\\', "\\\\")
            .replace('\'', "''");
        writeln!(std::fs::OpenOptions::new().append(true).open(data.join("postgresql.conf")).unwrap(),
            "\nlisten_addresses = ''\nunix_socket_directories = '{socket}'\nlogging_collector = on\nlog_destination = 'jsonlog'\nlog_directory = 'logs'\nlog_filename = 'deadlocks.log'\nlog_timezone = 'UTC'\ndeadlock_timeout = '100ms'").unwrap();
        cluster.command(
            "pg_ctl",
            &[
                "-D",
                data.to_str().unwrap(),
                "-l",
                cluster.0.path().join("startup.log").to_str().unwrap(),
                "-w",
                "start",
            ],
        );
        cluster
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new("pg_ctl")
            .args([
                "-D",
                self.0.path().join("data").to_str().unwrap(),
                "-m",
                "immediate",
                "-w",
                "stop",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[tokio::test]
async fn native_deadlock_json_is_projected_through_managed_history() {
    let cluster = Cluster::start();
    cluster.sql("CREATE TABLE deadlock_rows(id int PRIMARY KEY, value int); INSERT INTO deadlock_rows VALUES (1,0),(2,0)");
    let mut first = cluster.locked_row(1);
    let mut second = cluster.locked_row(2);
    writeln!(
        first.stdin.take().unwrap(),
        "UPDATE deadlock_rows SET value=1 WHERE id=2; COMMIT;"
    )
    .unwrap();
    writeln!(
        second.stdin.take().unwrap(),
        "UPDATE deadlock_rows SET value=2 WHERE id=1; COMMIT;"
    )
    .unwrap();
    let first = first.wait().unwrap();
    let second = second.wait().unwrap();
    assert_ne!(
        first.success(),
        second.success(),
        "exactly one native transaction must be a deadlock victim"
    );
    let relative_log = cluster.sql("SELECT pg_current_logfile('jsonlog')");
    let log = cluster.0.path().join("data").join(relative_log);
    // The logging collector is asynchronous; bound its flush wait separately
    // from the deadlock itself, which uses explicit locked-row barriers.
    for _ in 0..40 {
        if std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .any(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .is_some_and(|record| record["state_code"] == "40P01")
            })
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("native deadlock fixture").unwrap();
    let host = cluster.0.path().to_str().unwrap();
    let profile = metadata.upsert_connection_profile(TenantId(1), PrincipalId(1), NewConnectionProfile {
        name:"native deadlock".into(), provider_id:Engine::Postgres.provider_id(),
        configuration:serde_json::json!({"host":host,"port":5432,"database":"postgres","user":"sift","ssl_mode":"disable"}),
        semantic_engine:Some(Engine::Postgres), credentials:None, credential_mode:CredentialMode::Shared, tags:vec![],
    }).await.unwrap();
    let store = SessionStore::new(
        DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
    );
    store.set_authorization_store(metadata);
    store
        .configure_postgres_deadlock_logs(vec![PostgresDeadlockLogSource {
            tenant_id: 1,
            profile_id: profile.id.0,
            host: host.into(),
            port: Some(5432),
            database: "postgres".into(),
            path: log.to_str().unwrap().into(),
        }])
        .unwrap();
    let session = store
        .open_session_with_owner(
            OpenSessionRequest {
                tag: None,
                tenant_id: None,
            },
            Some(PrincipalId(1)),
            Some(TenantId(1)),
            false,
        )
        .unwrap()
        .id;
    let connection = store
        .open_managed_connection(
            session,
            Engine::Postgres.provider_id(),
            profile.configuration,
            Default::default(),
            PrincipalId(1),
            TenantId(1),
            profile.id,
            profile.policy.revision,
            true,
        )
        .await
        .unwrap()
        .id;
    let events = sift_server::process::list_deadlocks(&store, session, connection)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].participants.len(), 2);
    assert_eq!(
        events[0].participants.iter().filter(|p| p.victim).count(),
        1
    );
    assert!(events[0].participants.iter().any(|p| p
        .wait_resource
        .as_deref()
        .is_some_and(|r| r.starts_with("transaction "))));
    let summary = serde_json::to_string(&events).unwrap();
    assert!(!summary.contains("deadlock_rows"));
    assert!(!summary.contains("UPDATE"));
    store.close_session(session).unwrap();
}
