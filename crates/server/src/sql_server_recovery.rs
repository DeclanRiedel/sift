//! Copy-only backups and restore-to-new-name, using existing supervised execution.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sift_protocol::{
    ConnectionId, Engine, ExecuteRequestHttp, ExecuteResponse, OperationKind, Row, SessionId,
    SqlServerRecoveryReport, SqlServerRecoveryRequest, Value,
};
use std::collections::BTreeSet;

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}
fn validate_name(name: &str) -> ApiResult<()> {
    if name.is_empty()
        || name.encode_utf16().count() > 128
        || name.trim() != name
        || name.chars().any(char::is_control)
        || ["master", "model", "msdb", "tempdb"]
            .iter()
            .any(|system| name.eq_ignore_ascii_case(system))
    {
        return Err(bad(
            "recovery requires an explicit non-system database name of 1..128 UTF-16 units",
        ));
    }
    Ok(())
}
fn path_key(path: &str) -> ApiResult<String> {
    let normalized = path.replace('\\', "/");
    let prefix_parts = if normalized.starts_with("//") {
        2
    } else if normalized.starts_with('/') {
        1
    } else {
        0
    };
    let absolute = normalized.starts_with('/')
        || (normalized.as_bytes().get(1) == Some(&b':')
            && normalized.as_bytes().get(2) == Some(&b'/')
            && normalized.as_bytes()[0].is_ascii_alphabetic());
    if !absolute
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || normalized.ends_with('/')
        || normalized.split('/').skip(prefix_parts).any(|part| {
            part.is_empty() || part == "." || part == ".." || part.ends_with([' ', '.'])
        })
    {
        return Err(bad("recovery paths must be explicit absolute server-side file paths without dot segments or trailing spaces/dots"));
    }
    Ok(normalized.to_lowercase())
}

async fn execute(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: String,
) -> ApiResult<ExecuteResponse> {
    store
        .execute_http(
            session,
            ExecuteRequestHttp {
                connection,
                sql,
                params: vec![],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
        )
        .await
}
fn field<'a>(result: &ExecuteResponse, row: &'a Row, name: &str) -> ApiResult<&'a Value> {
    let index = result
        .columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| bad("native recovery metadata is missing a required column"))?;
    row.values
        .get(index)
        .ok_or_else(|| bad("native recovery metadata row is incomplete"))
}
fn number(value: &Value) -> Option<i64> {
    match value {
        Value::Int16(n) => Some(i64::from(*n)),
        Value::Int32(n) => Some(i64::from(*n)),
        Value::Int64(n) => Some(*n),
        _ => None,
    }
}

pub async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: SqlServerRecoveryRequest,
) -> ApiResult<SqlServerRecoveryReport> {
    let (database, archive_path, apply) = match &request {
        SqlServerRecoveryRequest::Backup {
            database,
            archive_path,
            apply,
        }
        | SqlServerRecoveryRequest::Restore {
            database,
            archive_path,
            apply,
            ..
        } => (database, archive_path, *apply),
    };
    validate_name(database)?;
    let archive_key = path_key(archive_path)?;
    let entry = store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteQuery,
        None,
        &[],
    )?;
    if entry.driver.engine() != Engine::SqlServer {
        return Err(sift_protocol::DriverError::new(
            sift_protocol::Code::UnsupportedForEngine,
            "SQL Server recovery requires SQL Server",
        )
        .into());
    }
    store.validate_execute_tx(session, connection, None)?;
    let database_sql = literal(database);
    let database_identifier = format!("[{}]", database.replace(']', "]]"));
    let archive_sql = literal(archive_path);
    let state=execute(store,session,connection,format!("SELECT DB_NAME() AS current_database, CONVERT(bigint,SERVERPROPERTY('ProductMajorVersion')) AS major_version, CONVERT(bigint,DB_ID({database_sql})) AS target_id, CONVERT(bigint,HAS_PERMS_BY_NAME(NULL,NULL,'VIEW ANY DATABASE')) AS can_view_databases")).await?;
    let row = state
        .rows
        .first()
        .ok_or_else(|| bad("native database preflight returned no row"))?;
    if field(&state, row, "current_database")? != &Value::Text("master".into())
        || number(field(&state, row, "can_view_databases")?) != Some(1)
    {
        return Err(bad(
            "SQL Server recovery requires a master connection with VIEW ANY DATABASE",
        ));
    }
    let target_id = number(field(&state, row, "target_id")?);
    let server_major = number(field(&state, row, "major_version")?)
        .ok_or_else(|| bad("missing server major version"))?;
    let (sql, source_database) = match &request {
        SqlServerRecoveryRequest::Backup { .. } => {
            if !target_id.is_some_and(|id| id > 4) {
                return Err(bad("backup source must be an existing user database"));
            }
            (format!("BACKUP DATABASE {database_identifier} TO DISK = {archive_sql} WITH COPY_ONLY, CHECKSUM, NOINIT, NOSKIP"),None)
        }
        SqlServerRecoveryRequest::Restore {
            backup_set, moves, ..
        } => {
            if target_id.is_some() {
                return Err(bad("restore destination already exists"));
            }
            if !(1..=65535).contains(backup_set) || moves.is_empty() || moves.len() > 128 {
                return Err(bad(
                    "restore requires a backup set 1..65535 and 1..128 explicit file moves",
                ));
            }
            let mut logical = BTreeSet::new();
            let mut physical = BTreeSet::new();
            for file in moves {
                if file.logical_name.is_empty()
                    || file.logical_name.encode_utf16().count() > 128
                    || file.logical_name.chars().any(char::is_control)
                    || !logical.insert(file.logical_name.clone())
                {
                    return Err(bad("restore logical file names must be valid and unique"));
                }
                let key = path_key(&file.destination)?;
                if key == archive_key || !physical.insert(key) {
                    return Err(bad(
                        "restore destinations must be distinct from each other and the archive",
                    ));
                }
            }
            let header = execute(
                store,
                session,
                connection,
                format!("RESTORE HEADERONLY FROM DISK = {archive_sql} WITH FILE = {backup_set}"),
            )
            .await?;
            let [row] = header.rows.as_slice() else {
                return Err(bad("restore requires exactly one selected backup header"));
            };
            if number(field(&header, row, "BackupType")?) != Some(1)
                || field(&header, row, "IsDamaged")? != &Value::Bool(false)
                || field(&header, row, "HasBackupChecksums")? != &Value::Bool(true)
            {
                return Err(bad(
                    "restore requires an undamaged full database backup with checksums",
                ));
            }
            if number(field(&header, row, "SoftwareVersionMajor")?)
                .map_or(true, |major| major > server_major)
            {
                return Err(bad("backup requires a newer SQL Server version"));
            }
            let Value::Text(source) = field(&header, row, "DatabaseName")? else {
                return Err(bad("backup source database is missing"));
            };
            let source = source.clone();
            // Compare under the server's identifier collation, not Rust casing.
            let same=execute(store,session,connection,format!("SELECT CONVERT(bigint,CASE WHEN {database_sql} = {} THEN 1 ELSE 0 END) AS same_name",literal(&source))).await?;
            if same
                .rows
                .first()
                .and_then(|r| r.values.first())
                .and_then(number)
                != Some(0)
            {
                return Err(bad(
                    "restore must use a new database name distinct from the archive source",
                ));
            }
            let files = execute(
                store,
                session,
                connection,
                format!("RESTORE FILELISTONLY FROM DISK = {archive_sql} WITH FILE = {backup_set}"),
            )
            .await?;
            let mut native = BTreeSet::new();
            for row in &files.rows {
                let Value::Text(name) = field(&files, row, "LogicalName")? else {
                    return Err(bad("backup file name is missing"));
                };
                if !matches!(field(&files,row,"Type")?,Value::Text(kind) if kind=="D" || kind=="L")
                {
                    return Err(bad(
                        "restore supports data/log files only, not FILESTREAM or other file types",
                    ));
                }
                native.insert(name.clone());
            }
            if native != logical {
                return Err(bad(
                    "every backup file requires exactly one explicit MOVE destination",
                ));
            }
            let move_sql = moves
                .iter()
                .map(|file| {
                    format!(
                        "MOVE {} TO {}",
                        literal(&file.logical_name),
                        literal(&file.destination)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let options = format!("FILE = {backup_set}, CHECKSUM, STOP_ON_ERROR, {move_sql}");
            execute(
                store,
                session,
                connection,
                format!("RESTORE VERIFYONLY FROM DISK = {archive_sql} WITH {options}"),
            )
            .await?;
            // No REPLACE: the distinct source/target names preserve SQL Server's
            // native existing-database safeguard even if another client races us.
            (format!("IF DB_ID({database_sql}) IS NOT NULL THROW 50000, 'Restore destination already exists', 1; RESTORE DATABASE {database_identifier} FROM DISK = {archive_sql} WITH {options}, RECOVERY"),Some(source))
        }
    };
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteQuery,
        Some(&sql),
        &[],
    )?;
    let warnings = if apply {
        execute(store, session, connection, sql.clone())
            .await?
            .warnings
    } else {
        vec![]
    };
    Ok(SqlServerRecoveryReport {
        applied: apply,
        sql,
        source_database,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_driver_api::mock::MockDriver;
    use sift_protocol::*;

    fn pages(names: &[&str], values: Vec<Vec<Value>>) -> Vec<Page> {
        vec![
            Page::NextResult {
                columns: names
                    .iter()
                    .map(|name| ColumnMetadata::new(*name, TypeRef::Primitive(PrimitiveType::Text)))
                    .collect(),
            },
            Page::Rows {
                rows: values.into_iter().map(Row::new).collect(),
            },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ]
    }
    fn preflight(id: Value) -> Vec<Page> {
        pages(
            &[
                "current_database",
                "major_version",
                "target_id",
                "can_view_databases",
            ],
            vec![vec![
                Value::Text("master".into()),
                Value::Int64(16),
                id,
                Value::Int64(1),
            ]],
        )
    }
    async fn connection(driver: MockDriver) -> (SessionStore, SessionId, ConnectionId) {
        let store = SessionStore::new(crate::DriverRegistry::builder().register(driver).build());
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let connection = store
            .open_connection(
                session,
                Engine::SqlServer,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: Some("master".into()),
                    user: "test".into(),
                    password: None,
                    ssl_mode: None,
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        (store, session, connection)
    }
    #[test]
    fn recovery_paths_and_names_are_explicit_and_literals_escape_quotes() {
        for path in [
            "relative.bak",
            "/data/../other",
            "/data//same",
            "/data/.",
            "C:relative",
            "/data/bad\0",
        ] {
            assert!(path_key(path).is_err());
        }
        assert!(path_key("C:\\backups\\safe.bak").is_ok());
        assert!(path_key("/backups/safe.bak").is_ok());
        assert_eq!(
            literal("x'; DROP DATABASE db;--"),
            "N'x''; DROP DATABASE db;--'"
        );
        for name in ["master", "", "bad\nname", "with trailing "] {
            assert!(validate_name(name).is_err());
        }
    }
    #[tokio::test]
    async fn backup_preview_does_not_execute_and_existing_restore_is_rejected() {
        let (store, session, conn) = connection(
            MockDriver::builder()
                .engine(Engine::SqlServer)
                .execute_ok(preflight(Value::Int64(5)))
                .execute_ok(preflight(Value::Int64(6)))
                .build(),
        )
        .await;
        let report = run(
            &store,
            session,
            conn,
            SqlServerRecoveryRequest::Backup {
                database: "odd]database".into(),
                archive_path: "/backups/a.bak".into(),
                apply: false,
            },
        )
        .await
        .unwrap();
        assert!(!report.applied);
        assert!(report.sql.contains("[odd]]database]"));
        assert!(report.sql.contains("COPY_ONLY, CHECKSUM, NOINIT, NOSKIP"));
        assert!(run(
            &store,
            session,
            conn,
            SqlServerRecoveryRequest::Restore {
                database: "restored".into(),
                archive_path: "/backups/a.bak".into(),
                backup_set: 1,
                moves: vec![],
                apply: true
            }
        )
        .await
        .is_err());
    }
    #[tokio::test]
    async fn restore_preview_checks_header_files_and_verify_without_replace() {
        let driver = MockDriver::builder()
            .engine(Engine::SqlServer)
            .execute_ok(preflight(Value::Null))
            .execute_ok(pages(
                &[
                    "BackupType",
                    "IsDamaged",
                    "HasBackupChecksums",
                    "SoftwareVersionMajor",
                    "DatabaseName",
                ],
                vec![vec![
                    Value::Int16(1),
                    Value::Bool(false),
                    Value::Bool(true),
                    Value::Int32(16),
                    Value::Text("source".into()),
                ]],
            ))
            .execute_ok(pages(&["same_name"], vec![vec![Value::Int64(0)]]))
            .execute_ok(pages(
                &["LogicalName", "Type"],
                vec![
                    vec![Value::Text("data".into()), Value::Text("D".into())],
                    vec![Value::Text("log".into()), Value::Text("L".into())],
                ],
            ))
            .execute_ok(vec![Page::Done {
                affected_rows: None,
                warnings: vec![],
            }])
            .build();
        let (store, session, conn) = connection(driver).await;
        let report = run(
            &store,
            session,
            conn,
            SqlServerRecoveryRequest::Restore {
                database: "restored".into(),
                archive_path: "/backups/a.bak".into(),
                backup_set: 1,
                moves: vec![
                    RestoreFileMove {
                        logical_name: "data".into(),
                        destination: "/data/restored.mdf".into(),
                    },
                    RestoreFileMove {
                        logical_name: "log".into(),
                        destination: "/data/restored.ldf".into(),
                    },
                ],
                apply: false,
            },
        )
        .await
        .unwrap();
        assert!(!report.applied);
        assert_eq!(report.source_database.as_deref(), Some("source"));
        assert!(!report.sql.contains("REPLACE"));
        assert!(report.sql.contains("MOVE N'data' TO N'/data/restored.mdf'"));
        assert!(report
            .sql
            .contains("IF DB_ID(N'restored') IS NOT NULL THROW"));
    }

    #[cfg(all(unix, feature = "live-mssql"))]
    #[tokio::test]
    async fn disposable_sql_server_backup_restore_integrity_and_resume() {
        use std::process::{Command, Stdio};
        let container_name = format!("sift-recovery-test-{}", uuid::Uuid::new_v4().simple());
        // Fixture-only credential. Never printed, persisted, or passed in argv.
        let password = format!("Test!{}", uuid::Uuid::new_v4().simple());
        let status = Command::new("docker")
            .args([
                "run",
                "--pull=never",
                "--rm",
                "-d",
                "--name",
                &container_name,
                "-p",
                "127.0.0.1::1433",
                "-e",
                "ACCEPT_EULA=Y",
                "-e",
                "MSSQL_PID=Developer",
                "-e",
                "MSSQL_SA_PASSWORD",
                "mcr.microsoft.com/mssql/server:2022-latest",
            ])
            .env("MSSQL_SA_PASSWORD", &password)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "disposable test requires Docker and the locally installed SQL Server 2022 image"
        );
        struct Container(String);
        impl Drop for Container {
            fn drop(&mut self) {
                let _ = Command::new("docker")
                    .args(["rm", "-f", &self.0])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _container = Container(container_name.clone());
        let output = Command::new("docker")
            .args(["port", &container_name, "1433/tcp"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let binding = String::from_utf8(output.stdout).unwrap();
        let port = binding
            .trim()
            .strip_prefix("127.0.0.1:")
            .unwrap()
            .parse()
            .unwrap();
        let store = SessionStore::new(
            crate::DriverRegistry::builder()
                .register(sift_driver_sqlserver::MssqlDriver::new())
                .build(),
        );
        store.set_request_timeout(std::time::Duration::from_secs(120));
        let session = store
            .open_session(OpenSessionRequest {
                tag: Some("disposable-recovery".into()),
                tenant_id: None,
            })
            .id;
        let mut spec = ConnectionSpec {
            host: "127.0.0.1".into(),
            port: Some(port),
            database: Some("master".into()),
            user: "sa".into(),
            password: Some(password),
            ssl_mode: Some(SslMode::Disable),
            engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
                trust_server_certificate: Some(true),
                ..Default::default()
            })),
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        let connection = loop {
            if let Ok(Ok(connection)) = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                store.open_connection(session, Engine::SqlServer, spec.clone()),
            )
            .await
            {
                break connection.id;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "disposable SQL Server did not become ready"
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        };
        let source = format!("sift_source_{}", uuid::Uuid::new_v4().simple());
        let target = format!("sift_restored_{}", uuid::Uuid::new_v4().simple());
        execute(
            &store,
            session,
            connection,
            format!("CREATE DATABASE [{source}]"),
        )
        .await
        .unwrap();
        execute(&store,session,connection,format!("CREATE TABLE [{source}].dbo.sample(id bigint); INSERT INTO [{source}].dbo.sample VALUES(17)")).await.unwrap();
        let archive_path = format!("/var/opt/mssql/data/{source}.bak");
        let backup = |apply| SqlServerRecoveryRequest::Backup {
            database: source.clone(),
            archive_path: archive_path.clone(),
            apply,
        };
        assert!(
            !run(&store, session, connection, backup(false))
                .await
                .unwrap()
                .applied
        );
        assert!(
            run(&store, session, connection, backup(true))
                .await
                .unwrap()
                .applied
        );
        // A second backup appends a set, preserving the original one.
        assert!(
            run(&store, session, connection, backup(true))
                .await
                .unwrap()
                .applied
        );
        let headers = execute(
            &store,
            session,
            connection,
            format!("RESTORE HEADERONLY FROM DISK = {}", literal(&archive_path)),
        )
        .await
        .unwrap();
        assert_eq!(headers.rows.len(), 2);
        let files=execute(&store,session,connection,format!("SELECT name AS logical_name, type AS file_type FROM sys.master_files WHERE database_id=DB_ID({})",literal(&source))).await.unwrap();
        let moves = files
            .rows
            .iter()
            .map(|row| {
                let Value::Text(logical_name) = field(&files, row, "logical_name").unwrap() else {
                    panic!("missing logical file name")
                };
                let extension = if number(field(&files, row, "file_type").unwrap()) == Some(0) {
                    "mdf"
                } else {
                    "ldf"
                };
                RestoreFileMove {
                    logical_name: logical_name.clone(),
                    destination: format!("/var/opt/mssql/data/{target}.{extension}"),
                }
            })
            .collect::<Vec<_>>();
        let restore = |database: String, apply| SqlServerRecoveryRequest::Restore {
            database,
            archive_path: archive_path.clone(),
            backup_set: 1,
            moves: moves.clone(),
            apply,
        };
        assert!(
            run(&store, session, connection, restore(source.clone(), true))
                .await
                .is_err()
        );
        assert!(
            !run(&store, session, connection, restore(target.clone(), false))
                .await
                .unwrap()
                .applied
        );
        assert!(
            run(&store, session, connection, restore(target.clone(), true))
                .await
                .unwrap()
                .applied
        );
        assert!(
            run(&store, session, connection, restore(target.clone(), true))
                .await
                .is_err()
        );
        let restored = execute(
            &store,
            session,
            connection,
            format!("SELECT id FROM [{target}].dbo.sample"),
        )
        .await
        .unwrap();
        assert_eq!(restored.rows[0].values[0], Value::Int64(17));
        spec.database = Some(target);
        let target_connection = store
            .open_connection(session, Engine::SqlServer, spec.clone())
            .await
            .unwrap()
            .id;
        assert_eq!(
            crate::integrity::run(
                &store,
                session,
                target_connection,
                IntegrityCheckRequest::SqlServer {
                    physical_only: false
                }
            )
            .await
            .unwrap()
            .outcome,
            IntegrityOutcome::NoIssuesReported
        );
        crate::process::list(&store, session, connection)
            .await
            .unwrap();
        execute(
            &store,
            session,
            target_connection,
            "CREATE TABLE dbo.resume_rows(value bigint CONSTRAINT pause_resume CHECK(value<100))"
                .into(),
        )
        .await
        .unwrap();
        let request = CsvImportRequest {
            table: "dbo.resume_rows".into(),
            data: format!(
                "value\n{}",
                (0..150).map(|n| format!("{n}\n")).collect::<String>()
            )
            .into_bytes(),
            header: true,
            delimiter: ',',
            null_value: None,
            create_table: false,
            conflict_policy: CsvConflictPolicy::Abort,
            dry_run: false,
            resume_from_row: 0,
            type_mappings: Default::default(),
        };
        let run_id = "8433f24a-2465-4a26-a6f9-cb24012aee08";
        assert!(crate::csv_import::import_with_checkpoint(
            &store,
            session,
            target_connection,
            request.clone(),
            "dbo.resume_checkpoint",
            run_id,
            "fixture"
        )
        .await
        .is_err());
        let count = execute(
            &store,
            session,
            target_connection,
            "SELECT COUNT_BIG(*) FROM dbo.resume_rows".into(),
        )
        .await
        .unwrap();
        assert_eq!(count.rows[0].values[0], Value::Int64(100));
        execute(
            &store,
            session,
            target_connection,
            "ALTER TABLE dbo.resume_rows DROP CONSTRAINT pause_resume".into(),
        )
        .await
        .unwrap();
        let other = store
            .open_connection(session, Engine::SqlServer, spec)
            .await
            .unwrap()
            .id;
        let resumed = crate::csv_import::import_with_checkpoint(
            &store,
            session,
            other,
            request.clone(),
            "dbo.resume_checkpoint",
            run_id,
            "fixture",
        )
        .await
        .unwrap();
        assert_eq!((resumed.rows_inserted, resumed.resume_from_row), (50, 150));
        let replay = crate::csv_import::import_with_checkpoint(
            &store,
            session,
            other,
            request,
            "dbo.resume_checkpoint",
            run_id,
            "fixture",
        )
        .await
        .unwrap();
        assert_eq!(replay.rows_inserted, 0);
        store.close_session(session).unwrap();
    }
}
