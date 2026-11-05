//! Transaction-owned, explicit main-table statistics and main-index rebuilds.
use super::*;
use rusqlite::OptionalExtension;
use sift_driver_api::SqliteFileMaintenanceState;

pub(super) fn handles(action: &SqliteMaintenanceAction) -> bool {
    matches!(
        action,
        SqliteMaintenanceAction::Analyze { .. } | SqliteMaintenanceAction::Reindex { .. }
    )
}
struct Authority<'a>(&'a AtomicBool, bool);
impl<'a> Authority<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        Self(flag, flag.swap(true, Ordering::AcqRel))
    }
}
impl Drop for Authority<'_> {
    fn drop(&mut self) {
        self.0.store(self.1, Ordering::Release);
    }
}
pub(super) fn preview_snapshot(
    worker: &Worker,
    action: &SqliteMaintenanceAction,
) -> Result<String, DriverError> {
    let closing = worker.entry.closing.clone();
    let _progress = TransactionGuard {
        conn: &worker.conn,
        closing: closing.clone(),
        owns_transaction: false,
    };
    let _busy = security::bound_busy_wait(100);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    worker.conn.progress_handler(
        1000,
        Some(move || closing.load(Ordering::Acquire) || std::time::Instant::now() >= deadline),
    );
    snapshot(worker, action)
}

fn snapshot(worker: &Worker, action: &SqliteMaintenanceAction) -> Result<String, DriverError> {
    sift_core::sqlite_maintenance::sql(action)
        .map_err(|m| error(Code::InvalidParameterValue, m))?;
    let (name, kind) = match action {
        SqliteMaintenanceAction::Analyze { table } => (table, "table"),
        SqliteMaintenanceAction::Reindex { index } => (index, "index"),
        _ => unreachable!("object maintenance only"),
    };
    let _authority = Authority::new(&worker.internal);
    let target = worker
        .conn
        .query_row(
            "SELECT type,substr(name,1,256),substr(tbl_name,1,256) FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE",
            [name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(|| {
            error(
                Code::InvalidParameterValue,
                "SQLite maintenance target was not found",
            )
        })?;
    if target.1.len() > 255 || target.2.len() > 255 {
        return Err(error(
            Code::ResultTooLarge,
            "SQLite maintenance target or owning table name exceeds 255 bytes",
        ));
    }
    let ordinary = worker
        .conn
        .query_row(
            "SELECT type FROM pragma_table_list WHERE schema='main' AND name=?1",
            [&target.2],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)?;
    if target.0 != kind
        || ordinary.as_deref() != Some("table")
        || target.2.to_ascii_lowercase().starts_with("sqlite_")
    {
        return Err(error(Code::UnsupportedForEngine,"Maintenance requires an ordinary main table or its index; virtual, shadow, internal and other schemas are excluded"));
    }
    let version: i64 = worker
        .conn
        .query_row("PRAGMA main.schema_version", [], |row| row.get(0))
        .map_err(db_error)?;
    type Definition = (String, String, String, i64, Option<String>);
    let mut statement=worker.conn.prepare("SELECT type,name,tbl_name,rootpage,sql,(length(CAST(name AS BLOB))>255 OR length(CAST(tbl_name AS BLOB))>255 OR coalesce(length(CAST(sql AS BLOB)),0)>1048576) FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE OR tbl_name=?1 COLLATE NOCASE ORDER BY type,name LIMIT 202").map_err(db_error)?;
    let mut rows = statement.query([&target.2]).map_err(db_error)?;
    let mut definitions: Vec<Definition> = Vec::new();
    let mut bytes = 0;
    while let Some(row) = rows.next().map_err(db_error)? {
        if row.get::<_, bool>(5).map_err(db_error)? {
            return Err(error(
                Code::ResultTooLarge,
                "SQLite maintenance native definition exceeds bounds",
            ));
        }
        let definition: Definition = (
            row.get(0).map_err(db_error)?,
            row.get(1).map_err(db_error)?,
            row.get(2).map_err(db_error)?,
            row.get(3).map_err(db_error)?,
            row.get(4).map_err(db_error)?,
        );
        bytes += definition.0.len()
            + definition.1.len()
            + definition.2.len()
            + definition.4.as_ref().map_or(0, String::len);
        if bytes > 1024 * 1024 {
            return Err(error(
                Code::ResultTooLarge,
                "SQLite maintenance definitions exceed 1 MiB",
            ));
        }
        definitions.push(definition);
    }
    if definitions.len() > 201 {
        return Err(error(
            Code::ResultTooLarge,
            "SQLite maintenance target exceeds 201 native definitions",
        ));
    }
    let snapshot = serde_json::to_string(&(version, target, definitions)).map_err(|_| {
        error(
            Code::DriverInternal,
            "SQLite maintenance snapshot could not be encoded",
        )
    })?;
    if snapshot.len() > 1024 * 1024 {
        return Err(error(
            Code::ResultTooLarge,
            "SQLite maintenance snapshot exceeds 1 MiB",
        ));
    }
    Ok(snapshot)
}
pub(super) fn bind(state: &mut SqliteFileMaintenanceState, snapshot: &str) {
    state.identity.push_str("\nobject:");
    state
        .identity
        .push_str(&sift_core::sqlite_maintenance::definition_identity(
            snapshot,
        ));
}
struct TransactionGuard<'a> {
    conn: &'a Connection,
    closing: Arc<AtomicBool>,
    owns_transaction: bool,
}
impl Drop for TransactionGuard<'_> {
    fn drop(&mut self) {
        self.conn.progress_handler(0, None::<fn() -> bool>);
        if self.owns_transaction
            && !self.conn.is_autocommit()
            && self.conn.execute_batch("ROLLBACK").is_err()
        {
            self.closing.store(true, Ordering::Release);
        }
        let closing = self.closing.clone();
        self.conn
            .progress_handler(1000, Some(move || closing.load(Ordering::Acquire)));
    }
}
pub(super) fn apply(
    worker: &Worker,
    files: &FilePolicy,
    tenant: i64,
    action: &SqliteMaintenanceAction,
    expected: &str,
    cancel: Arc<AtomicBool>,
) -> Result<SqliteFileMaintenanceState, DriverError> {
    check_restore_safety(worker)?;
    check_vacuum_safety(worker)?;
    let sql = sift_core::sqlite_maintenance::sql(action)
        .map_err(|m| error(Code::InvalidParameterValue, m))?
        .ok_or_else(|| {
            error(
                Code::DriverInternal,
                "Missing SQLite object maintenance SQL",
            )
        })?;
    let _authority = Authority::new(&worker.internal);
    let _busy = security::bound_busy_wait(100);
    let closing = worker.entry.closing.clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let cancelled = || {
        cancel.load(Ordering::Acquire)
            || closing.load(Ordering::Acquire)
            || std::time::Instant::now() >= deadline
    };
    if cancelled() {
        return Err(error(
            Code::QueryCanceled,
            "SQLite object maintenance canceled",
        ));
    }
    let progress_cancel = cancel.clone();
    let progress_closing = closing.clone();
    let mut transaction = TransactionGuard {
        conn: &worker.conn,
        closing: closing.clone(),
        owns_transaction: false,
    };
    worker.conn.progress_handler(
        1000,
        Some(move || {
            progress_cancel.load(Ordering::Acquire)
                || progress_closing.load(Ordering::Acquire)
                || std::time::Instant::now() >= deadline
        }),
    );
    worker
        .conn
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(db_error)?;
    transaction.owns_transaction = true;
    if cancelled() {
        return Err(error(
            Code::QueryCanceled,
            "SQLite object maintenance canceled",
        ));
    }
    let mut target =
        files.inspect_maintenance(&worker.file_path, worker.read_only, tenant, action)?;
    bind(&mut target.state, &snapshot(worker, action)?);
    if target.state.identity != expected {
        return Err(error(
            Code::InvalidParameterValue,
            "SQLite object maintenance preview is stale",
        ));
    }
    worker.conn.execute_batch(&sql).map_err(db_error)?;
    if cancelled() {
        return Err(error(
            Code::QueryCanceled,
            "SQLite object maintenance canceled before commit",
        ));
    }
    worker.conn.execute_batch("COMMIT").map_err(db_error)?;
    worker.conn.flush_prepared_statement_cache();
    target.state.source_bytes = std::fs::metadata(&worker.file_path)
        .map(|m| m.len())
        .unwrap_or(target.state.source_bytes);
    Ok(target.state)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use sift_driver_api::SqliteExt;

    #[tokio::test]
    async fn interrupted_object_work_rolls_back_and_restores_worker_authority() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("source.db");
        let database = Connection::open(&file).unwrap();
        database.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY,v INTEGER); WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200000) INSERT INTO items SELECT i,i%1000 FROM n; CREATE INDEX items_v ON items(v);").unwrap();
        database.busy_timeout(Duration::ZERO).unwrap();
        let driver = Arc::new(SqliteDriver::with_files(FilePolicy {
            config: SqliteDriverConfig {
                roots: std::collections::BTreeMap::from([(
                    "test".into(),
                    SqliteRootConfig {
                        path: root.path().to_string_lossy().into_owned(),
                        allowed_tenants: vec![1],
                        read_only: false,
                    },
                )]),
                max_connections: 1,
            },
            protected: vec![],
        }));
        let handle = driver
            .open_file(
                SqliteFileConfiguration {
                    root_id: "test".into(),
                    path: "source.db".into(),
                    mode: SqliteOpenMode::ReadWrite,
                    busy_timeout_ms: 1000,
                },
                Some(1),
            )
            .await
            .unwrap();
        for action in [
            SqliteMaintenanceAction::Analyze {
                table: "items".into(),
            },
            SqliteMaintenanceAction::Reindex {
                index: "items_v".into(),
            },
        ] {
            let preview = driver
                .inspect_file_maintenance(handle.clone(), 1, action.clone())
                .await
                .unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let task_driver = driver.clone();
            let task_handle = handle.clone();
            let task_cancel = cancel.clone();
            let task = tokio::spawn(async move {
                task_driver
                    .apply_file_maintenance(task_handle, 1, action, preview.identity, task_cancel)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match database.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => database.execute_batch("ROLLBACK").unwrap(),
                        Err(failure)
                            if failure.sqlite_error_code()
                                == Some(rusqlite::ErrorCode::DatabaseBusy) =>
                        {
                            break
                        }
                        Err(failure) => panic!("unexpected lock observation: {failure}"),
                    }
                    assert!(
                        !task.is_finished(),
                        "native write transaction must be observed before completion"
                    );
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            cancel.store(true, Ordering::Release);
            driver.entry(&handle).unwrap().interrupt.interrupt();
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(result,Err(ref failure) if failure.code==Code::QueryCanceled),
                "{result:?}"
            );
            driver
                .run(handle.clone(), |worker| {
                    assert!(worker.conn.is_autocommit());
                    assert!(!worker.internal.load(Ordering::Acquire));
                    assert!(
                        worker
                            .conn
                            .prepare("ATTACH DATABASE ':memory:' AS escaped")
                            .is_err(),
                        "native authority must be restored"
                    );
                    assert_eq!(
                        worker
                            .conn
                            .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
                            .unwrap(),
                        200000
                    );
                    Ok(())
                })
                .await
                .unwrap();
            assert!(driver.ping(handle.clone()).await.is_ok());
            assert_eq!(
                database
                    .query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE name='sqlite_stat1'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0,
                "interrupted ANALYZE must roll back statistics creation"
            );
            assert_eq!(
                database
                    .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
        }
        // Native REINDEX must also unwind its write transaction on a real
        // uniqueness failure. Simulate externally changed index semantics.
        database.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='CREATE UNIQUE INDEX items_v ON items(v)' WHERE name='items_v'; PRAGMA schema_version=100; PRAGMA writable_schema=OFF;").unwrap();
        let action = SqliteMaintenanceAction::Reindex {
            index: "items_v".into(),
        };
        let preview = driver
            .inspect_file_maintenance(handle.clone(), 1, action.clone())
            .await
            .unwrap();
        assert!(driver
            .apply_file_maintenance(
                handle.clone(),
                1,
                action,
                preview.identity,
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err());
        driver
            .run(handle.clone(), |worker| {
                assert!(worker.conn.is_autocommit());
                assert!(!worker.internal.load(Ordering::Acquire));
                Ok(())
            })
            .await
            .unwrap();
        database.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='CREATE INDEX items_v ON items(v)' WHERE name='items_v'; PRAGMA schema_version=101; PRAGMA writable_schema=OFF;").unwrap();
        assert!(driver.ping(handle.clone()).await.is_ok());
        assert_eq!(
            database
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        driver.close(handle).await.unwrap();
    }
}
