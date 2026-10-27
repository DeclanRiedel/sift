//! Operator-configured PostgreSQL JSON log projection. Raw log text never leaves
//! this module or enters persistent Sift storage.
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use sift_instance_config::PostgresDeadlockLogSource;
use sift_protocol::{
    Code, ConnectionId, DatabaseDeadlockEvent, DatabaseDeadlockParticipant, DriverError, Engine,
    ExecuteRequestHttp, OperationKind, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::{ConnectionProvenance, SessionStore};

const MAX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LINE: usize = 64 * 1024;
const MAX_EVENTS: usize = 50;
const MAX_PARTICIPANTS: usize = 16;
const PERMISSION_SQL: &str = "SELECT (current_setting('is_superuser') = 'on' OR pg_catalog.pg_has_role(current_user, 'pg_read_all_stats', 'USAGE')), current_database()::text";

pub(crate) struct PostgresDeadlockLogs {
    sources: RwLock<Vec<PostgresDeadlockLogSource>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Default for PostgresDeadlockLogs {
    fn default() -> Self {
        Self {
            sources: RwLock::new(Vec::new()),
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}

impl PostgresDeadlockLogs {
    pub(crate) fn configure(&self, sources: Vec<PostgresDeadlockLogSource>) -> ApiResult<()> {
        PostgresDeadlockLogSource::validate_sources(&sources).map_err(|_| {
            ApiError::BadRequest("Invalid PostgreSQL log source configuration".into())
        })?;
        if sources
            .iter()
            .any(|source| !std::path::Path::new(&source.path).is_absolute())
        {
            return Err(ApiError::BadRequest(
                "PostgreSQL log sources require absolute server-local paths".into(),
            ));
        }
        *self.sources.write().unwrap() = sources;
        Ok(())
    }

    fn source(
        &self,
        store: &SessionStore,
        session: SessionId,
        connection: ConnectionId,
    ) -> ApiResult<PostgresDeadlockLogSource> {
        let entry = store.authorize_connection_operation(
            session,
            connection,
            OperationKind::ListDeadlocks,
            None,
            &[],
        )?;
        if store
            .current_connection_policy(session, connection)?
            .is_some_and(|policy| policy.allowed_schemas.is_some())
        {
            return Err(ApiError::Forbidden(
                "PostgreSQL deadlock history is unavailable for schema-restricted profiles".into(),
            ));
        }
        let ConnectionProvenance::Managed {
            tenant_id,
            profile_id,
            ..
        } = entry.provenance
        else {
            return Err(unavailable());
        };
        self.sources
            .read()
            .unwrap()
            .iter()
            .find(|source| {
                source.tenant_id == tenant_id.0
                    && source.profile_id == profile_id.0
                    && entry
                        .configuration
                        .get("host")
                        .and_then(serde_json::Value::as_str)
                        == Some(source.host.as_str())
                    && entry
                        .configuration
                        .get("port")
                        .and_then(serde_json::Value::as_u64)
                        == source.port.map(u64::from)
            })
            .cloned()
            .ok_or_else(unavailable)
    }
}

fn unavailable() -> ApiError {
    DriverError::new(
        Code::UnsupportedForEngine,
        "No matching operator-configured PostgreSQL JSON log source for this managed connection",
    )
    .into()
}

pub async fn list(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<Vec<DatabaseDeadlockEvent>> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(unavailable());
    }
    let logs = store.postgres_deadlock_logs();
    let source = logs.source(store, session, connection)?;
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: PERMISSION_SQL.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ListDeadlocks,
        )
        .await?;
    let values = response.rows.first().map(|row| row.values.as_slice());
    match values {
        Some([Value::Bool(true), Value::Text(database)]) if database == &source.database => {},
        _ => return Err(ApiError::Forbidden("PostgreSQL log history requires pg_read_all_stats or superuser in the configured database".into())),
    }
    let permit = logs
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::RateLimited {
            retry_after_secs: 1,
        })?;
    let read_source = source.clone();
    let task = tokio::task::spawn_blocking(move || {
        // Retain admission even if the HTTP waiter times out during filesystem IO.
        let _permit = permit;
        read(&read_source)
    });
    let events = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .map_err(|_| DriverError::new(Code::QueryTimedOut, "PostgreSQL log read timed out"))?
        .map_err(|_| ApiError::Internal("PostgreSQL log worker failed".into()))??;
    // A disconnected connection or revoked/profile-changed scope cannot receive
    // data merely because its filesystem read began while it was authorized.
    if logs.source(store, session, connection)? != source {
        return Err(unavailable());
    }
    Ok(events)
}

fn read(source: &PostgresDeadlockLogSource) -> ApiResult<Vec<DatabaseDeadlockEvent>> {
    let io_error = |_| {
        ApiError::BadRequest("Configured PostgreSQL log source is unavailable or unreadable".into())
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A misconfigured FIFO must not retain a worker waiting for a writer.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(&source.path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err(ApiError::BadRequest(
            "PostgreSQL log source must be a regular file".into(),
        ));
    }
    let start = metadata.len().saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(io_error)?;
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_BYTES) as usize);
    file.take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let bytes = if start > 0 {
        bytes
            .splitn(2, |byte| *byte == b'\n')
            .nth(1)
            .unwrap_or_default()
    } else {
        &bytes
    };
    Ok(project(bytes, &source.database))
}

#[derive(serde::Deserialize)]
struct LogLine {
    timestamp: String,
    pid: i64,
    dbname: String,
    state_code: String,
    #[serde(default)]
    detail: String,
}

fn project(bytes: &[u8], database: &str) -> Vec<DatabaseDeadlockEvent> {
    let mut events = VecDeque::with_capacity(MAX_EVENTS);
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| line.len() <= MAX_LINE)
    {
        let Ok(record) = serde_json::from_slice::<LogLine>(line) else {
            continue;
        };
        if record.state_code != "40P01" || record.dbname != database || !valid_pid(record.pid) {
            continue;
        }
        let Some(occurred_at) = timestamp(&record.timestamp) else {
            continue;
        };
        let (participants, participants_truncated) = participants(record.pid, &record.detail);
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(DatabaseDeadlockEvent {
            occurred_at,
            participants,
            participants_truncated,
        });
    }
    let mut events = events.into_iter().collect::<Vec<_>>();
    events.sort_by_key(|event| std::cmp::Reverse(event.occurred_at));
    events
}

fn timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(time.to_utc());
    }
    if let Some(value) = value.strip_suffix(" UTC") {
        return chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .map(|time| time.and_utc());
    }
    chrono::DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f%#z")
        .ok()
        .map(|time| time.to_utc())
}

fn valid_pid(pid: i64) -> bool {
    (1..=i64::from(i32::MAX)).contains(&pid)
}

fn participants(victim: i64, detail: &str) -> (Vec<DatabaseDeadlockParticipant>, bool) {
    static WAIT: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
        r"\AProcess ([1-9][0-9]{0,9}) waits for ([A-Za-z]{1,32}) on (transaction [0-9]{1,20}|relation [0-9]{1,20} of database [0-9]{1,20}|virtual transaction [0-9]{1,10}/[0-9]{1,10}|tuple \([0-9]{1,20},[0-9]{1,20}\) of relation [0-9]{1,20} of database [0-9]{1,20}); blocked by process ([1-9][0-9]{0,9})\.\z"
    ).expect("static PostgreSQL deadlock grammar")
    });
    let make = |pid| DatabaseDeadlockParticipant {
        process_id: pid,
        victim: pid == victim,
        wait_resource: None,
        lock_mode: None,
        wait_ms: None,
    };
    let mut entries = BTreeMap::from([(victim, make(victim))]);
    let mut truncated = false;
    let mut parsed_waits = 0;
    for line in detail.lines() {
        let Some(captures) = WAIT.captures(line) else {
            truncated |= line.starts_with("Process ") && line.contains(" waits for ");
            continue;
        };
        if !matches!(
            &captures[2],
            "AccessShareLock"
                | "RowShareLock"
                | "RowExclusiveLock"
                | "ShareUpdateExclusiveLock"
                | "ShareLock"
                | "ShareRowExclusiveLock"
                | "ExclusiveLock"
                | "AccessExclusiveLock"
                | "SIReadLock"
        ) {
            truncated = true;
            continue;
        }
        let Ok(pid) = captures[1].parse::<i64>() else {
            continue;
        };
        let Ok(blocker) = captures[4].parse::<i64>() else {
            continue;
        };
        if !valid_pid(pid) || !valid_pid(blocker) {
            truncated = true;
            continue;
        }
        parsed_waits += 1;
        for id in [pid, blocker] {
            if !entries.contains_key(&id) && entries.len() == MAX_PARTICIPANTS {
                truncated = true;
                continue;
            }
            let participant = entries.entry(id).or_insert_with(|| make(id));
            if id == pid {
                participant.wait_resource = Some(captures[3].to_owned());
                participant.lock_mode = Some(captures[2].to_owned());
            }
        }
    }
    (
        entries.into_values().collect(),
        truncated || parsed_waits == 0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: i64, database: &str) -> serde_json::Value {
        serde_json::json!({
            "timestamp":"2026-10-27 10:11:12.123 UTC", "pid":pid,
            "dbname":database, "state_code":"40P01",
            "detail":format!("Process {pid} waits for ShareLock on transaction 42; blocked by process 99.\nProcess 99 waits for ExclusiveLock on relation 17 of database 5; blocked by process {pid}.\nProcess {pid}: SECRET SQL"),
            "statement":"SECRET SQL", "message":"SECRET SQL", "context":"SECRET SQL",
        })
    }

    #[test]
    fn projection_filters_database_and_never_returns_sql_or_arbitrary_detail() {
        let input = format!(
            "{}\n{}\nmalformed\n{{\"state_code\":\"40P01\"}}\n",
            record(41, "app"),
            record(51, "other")
        );
        let events = project(input.as_bytes(), "app");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].participants.len(), 2);
        assert!(events[0].participants[0].victim);
        assert_eq!(
            events[0].participants[0].wait_resource.as_deref(),
            Some("transaction 42")
        );
        assert_eq!(
            events[0].participants[1].lock_mode.as_deref(),
            Some("ExclusiveLock")
        );
        assert!(!serde_json::to_string(&events).unwrap().contains("SECRET"));
        assert_eq!(
            timestamp("2026-10-27 12:11:12.123+02").unwrap(),
            events[0].occurred_at
        );
        assert!(timestamp("2026-10-27 10:11:12 PST").is_none());
        let (participants, _) = participants(
            41,
            "Process 41 waits for SELECT on transaction 42; blocked by process 99.",
        );
        assert_eq!(participants.len(), 1);
    }

    #[test]
    fn projection_bounds_events_lines_and_participants() {
        let input = (1..=70)
            .map(|pid| record(pid, "app").to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let events = project(input.as_bytes(), "app");
        assert_eq!(events.len(), MAX_EVENTS);
        assert!(!events.iter().any(|event| event
            .participants
            .iter()
            .any(|p| p.victim && p.process_id <= 20)));
        let detail = (1..=30)
            .map(|pid| {
                format!(
                    "Process {pid} waits for ShareLock on transaction 42; blocked by process 99."
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (participants, truncated) = participants(100, &detail);
        assert_eq!(participants.len(), MAX_PARTICIPANTS);
        assert!(truncated);
        assert!(participants.iter().any(|p| p.victim && p.process_id == 100));
        let mut oversized = record(41, "app");
        oversized["statement"] = "x".repeat(MAX_LINE).into();
        assert!(project(oversized.to_string().as_bytes(), "app").is_empty());
    }

    fn source(path: String) -> PostgresDeadlockLogSource {
        PostgresDeadlockLogSource {
            tenant_id: 1,
            profile_id: 1,
            host: "mock".into(),
            port: None,
            database: "app".into(),
            path,
        }
    }

    #[test]
    fn file_read_is_bounded_and_configuration_rejects_ambiguous_sources() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", record(1, "app")).unwrap();
        file.as_file_mut()
            .seek(SeekFrom::Start(MAX_BYTES + 100))
            .unwrap();
        writeln!(file, "\n{}", record(41, "app")).unwrap();
        let config = source(file.path().to_str().unwrap().into());
        let events = read(&config).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0]
            .participants
            .iter()
            .any(|p| p.process_id == 41 && p.victim));
        let logs = PostgresDeadlockLogs::default();
        assert!(logs.configure(vec![config.clone(), config]).is_err());
        assert!(logs
            .configure(vec![source("relative.json".into())])
            .is_err());
    }

    #[tokio::test]
    async fn managed_scope_database_privilege_and_source_identity_are_required() {
        use crate::DriverRegistry;
        use sift_driver_api::mock::MockDriver;
        use sift_metadata::{
            CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId,
            TenantId,
        };
        use sift_protocol::{OpenSessionRequest, Page, Row, ServerInfo};

        for (privilege, database, host, allowed) in [
            (true, "app", "mock", true),
            (false, "app", "mock", false),
            (true, "other", "mock", false),
            (true, "app", "wrong-host", false),
        ] {
            let driver = MockDriver::builder()
                .engine(Engine::Postgres)
                .ping_ok(ServerInfo {
                    provider: Engine::Postgres.provider_ref("test"),
                    server_version: "mock".into(),
                    current_database: "app".into(),
                    current_user: "alice".into(),
                    pool_warm_slots: None,
                })
                .execute_ok(vec![
                    Page::Rows {
                        rows: vec![Row::new(vec![
                            Value::Bool(privilege),
                            Value::Text(database.into()),
                        ])],
                    },
                    Page::Done {
                        affected_rows: None,
                        warnings: vec![],
                    },
                ])
                .build();
            let metadata =
                MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
            metadata.bootstrap_local("log history test").unwrap();
            let profile = metadata.upsert_connection_profile(TenantId(1), PrincipalId(1), NewConnectionProfile {
                name:"test".into(), provider_id:Engine::Postgres.provider_id(), configuration:serde_json::json!({"host":"mock","database":"app","user":"alice"}),
                semantic_engine:Some(Engine::Postgres), credentials:None, credential_mode:CredentialMode::Shared, tags:vec![],
            }).await.unwrap();
            let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
            store.set_authorization_store(metadata.clone());
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
                    profile.configuration.clone(),
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
            let mut file = tempfile::NamedTempFile::new().unwrap();
            use std::io::Write;
            writeln!(file, "{}", record(41, "app")).unwrap();
            let mut config = source(file.path().to_str().unwrap().into());
            config.profile_id = profile.id.0;
            config.host = host.into();
            store
                .configure_postgres_deadlock_logs(vec![config])
                .unwrap();
            let result = list(&store, session, connection).await;
            if allowed {
                assert_eq!(result.unwrap().len(), 1);
                metadata
                    .update_connection_policy(
                        TenantId(1),
                        PrincipalId(1),
                        profile.id,
                        sift_protocol::UpdateConnectionPolicyRequest {
                            expected_revision: Some(profile.policy.revision),
                            minimum_tenant_role: sift_protocol::TenantRole::Member,
                            read_only: false,
                            allowed_ops: None,
                            blocked_ops: vec![],
                            allowed_schemas: Some(vec![sift_protocol::SchemaSelector {
                                catalog: None,
                                schema: "public".into(),
                            }]),
                        },
                        sift_metadata::NewOperationAudit {
                            actor_principal_id: Some(PrincipalId(1)),
                            action: "update".into(),
                            target: "connection_policy".into(),
                            target_id: Some(profile.id.0),
                            status: "succeeded".into(),
                            result_code: None,
                            row_count: None,
                            error_message: None,
                            correlation_id: None,
                        },
                    )
                    .unwrap();
                assert!(matches!(
                    list(&store, session, connection).await,
                    Err(ApiError::Forbidden(_))
                ));
            } else {
                assert!(result.is_err());
            }
        }
    }
}
