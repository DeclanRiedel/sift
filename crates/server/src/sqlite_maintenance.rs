//! Preview-bound SQLite file creation, online backup, and VACUUM (ADRs 078, 085).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use sift_protocol::{
    Code, ConnectionId, DriverError, Engine, OperationKind, SessionId, SqliteMaintenanceAction,
    SqliteMaintenanceReport, SqliteMaintenanceRequest,
};

use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: SqliteMaintenanceRequest,
) -> ApiResult<SqliteMaintenanceReport> {
    let (_, tenant, _, _) =
        store.managed_catalog_scope(session, connection, OperationKind::ManageSqliteDatabase)?;
    let entry = store.conn_entry(session, connection)?;
    if entry.driver.engine() != Engine::Sqlite {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "SQLite file maintenance requires a managed SQLite connection",
        )
        .into());
    }
    store.validate_execute_tx(session, connection, None)?;
    let driver = entry.driver.clone();
    let handle = entry
        .handle
        .builtin()
        .cloned()
        .ok_or_else(|| ApiError::BadRequest("native SQLite connection required".into()))?;
    let action = request.action.clone();
    let state = store
        .run_bounded("SQLite file maintenance preview", {
            let driver = driver.clone();
            let handle = handle.clone();
            let action = action.clone();
            async move {
                driver
                    .as_sqlite()
                    .ok_or_else(|| {
                        DriverError::new(
                            Code::UnsupportedForEngine,
                            "SQLite native file maintenance unavailable",
                        )
                    })?
                    .inspect_file_maintenance(handle, tenant.0, action)
                    .await
            }
        })
        .await?;
    let backup_file = matches!(&action, SqliteMaintenanceAction::Backup { .. })
        .then(|| state.destination_file.clone())
        .flatten();
    let backup_expectation = match &action {
        SqliteMaintenanceAction::Create { .. } => {
            "The new database is empty; back it up after adding data."
        }
        SqliteMaintenanceAction::Backup { .. } => {
            "Verify and retain this new backup before later source mutations."
        }
        SqliteMaintenanceAction::Vacuum => {
            "Verify a current backup before VACUUM; implicit ROWIDs may change on tables without an explicit INTEGER PRIMARY KEY. Apply requires your acknowledgment."
        }
    }
    .to_string();
    if !request.apply {
        if request.confirm_write || request.backup_verified || request.preview_token.is_some() {
            return Err(ApiError::BadRequest(
                "SQLite maintenance preview cannot include apply confirmation".into(),
            ));
        }
        let token = store.store_sqlite_maintenance_preview(
            session,
            connection,
            action.clone(),
            state.identity,
        )?;
        return Ok(SqliteMaintenanceReport {
            action,
            applied: false,
            root_id: state.root_id,
            source_file: state.source_file,
            destination_file: state.destination_file,
            source_bytes: state.source_bytes,
            estimated_extra_bytes: state.estimated_extra_bytes,
            backup_file,
            backup_expectation,
            preview_token: Some(token),
        });
    }
    if !request.confirm_write {
        return Err(ApiError::BadRequest(
            "confirm_write is required for SQLite file maintenance".into(),
        ));
    }
    if matches!(&action, SqliteMaintenanceAction::Vacuum) && !request.backup_verified {
        return Err(ApiError::BadRequest(
            "a verified backup acknowledgment is required before SQLite VACUUM".into(),
        ));
    }
    if !matches!(&action, SqliteMaintenanceAction::Vacuum) && request.backup_verified {
        return Err(ApiError::BadRequest(
            "backup_verified applies only to SQLite VACUUM".into(),
        ));
    }
    let token = request.preview_token.as_deref().ok_or_else(|| {
        ApiError::BadRequest("preview SQLite file maintenance before apply".into())
    })?;
    store.consume_sqlite_maintenance_preview(
        token,
        session,
        connection,
        &action,
        &state.identity,
    )?;
    let cancel = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    let expected = state.identity;
    let result = store
        .run_bounded("SQLite file maintenance apply", async move {
            driver
                .as_sqlite()
                .ok_or_else(|| {
                    DriverError::new(
                        Code::UnsupportedForEngine,
                        "SQLite native file maintenance unavailable",
                    )
                })?
                .apply_file_maintenance(handle, tenant.0, action.clone(), expected, cancel)
                .await
        })
        .await?;
    Ok(SqliteMaintenanceReport {
        action: request.action,
        applied: true,
        root_id: result.root_id,
        source_file: result.source_file,
        destination_file: result.destination_file,
        source_bytes: result.source_bytes,
        estimated_extra_bytes: result.estimated_extra_bytes,
        backup_file,
        backup_expectation,
        preview_token: None,
    })
}
