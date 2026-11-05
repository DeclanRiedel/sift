//! Reviewed single-index work on a disposable native session.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sha2::{Digest, Sha256};
use sift_protocol::*;
use tokio_util::sync::CancellationToken;
const ACTOR:&str="SELECT CONVERT(nvarchar(max),(SELECT CONVERT(nvarchar(128),SERVERPROPERTY('ServerName')) AS server_name,DB_ID() AS database_id,DB_NAME() AS database_name,CONVERT(varchar(172),SUSER_SID(),1) AS login_sid,USER_ID() AS user_id,(SELECT CONVERT(varchar(172),sid,1) FROM sys.database_principals WHERE principal_id=USER_ID()) AS user_sid,ORIGINAL_LOGIN() AS original_login,@@TRANCOUNT AS transactions FOR JSON PATH,WITHOUT_ARRAY_WRAPPER))";
const SETUP:&str="SET ROWCOUNT 0; SET NOCOUNT ON; SET ANSI_NULLS ON; SET ANSI_PADDING ON; SET ANSI_WARNINGS ON; SET ARITHABORT ON; SET CONCAT_NULL_YIELDS_NULL ON; SET QUOTED_IDENTIFIER ON; SET NUMERIC_ROUNDABORT OFF; SET XACT_ABORT ON; SET LOCK_TIMEOUT 5000";
fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
async fn query(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: &str,
    params: Vec<Value>,
) -> ApiResult<ExecuteResponse> {
    store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params,
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::MaintainSqlServerIndex,
        )
        .await
}
async fn actor(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<String> {
    let response = query(store, session, connection, ACTOR, vec![]).await?;
    let row = response
        .rows
        .first()
        .ok_or_else(|| bad("Native actor context is unavailable"))?;
    let [Value::Text(value)] = row.values.as_slice() else {
        return Err(bad("Native actor context is unavailable"));
    };
    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|_| bad("Invalid native actor context"))?;
    if parsed["transactions"] != 0 {
        return Err(bad(
            "Index maintenance requires no native source transaction",
        ));
    }
    Ok(value.clone())
}
pub async fn run(
    store: &SessionStore,
    session: SessionId,
    source: ConnectionId,
    request: SqlServerIndexRequest,
    cancel: CancellationToken,
) -> ApiResult<SqlServerIndexReport> {
    let sql = sift_core::sqlserver_index_maintenance::sql(&request).map_err(bad)?;
    if request.apply {
        if !request.production_confirmed
            || !request.backup_verified
            || request
                .precondition
                .as_ref()
                .map_or(true, |p| p.len() != 64)
        {
            return Err(bad("Current preview, production confirmation and verified-backup acknowledgment are required"));
        }
    } else if request.production_confirmed
        || request.backup_verified
        || request.precondition.is_some()
    {
        return Err(bad("Preview cannot include apply confirmation"));
    }
    let entry = store.authorize_connection_operation(
        session,
        source,
        OperationKind::MaintainSqlServerIndex,
        Some(&sql),
        &[],
    )?;
    if entry.driver.semantic_engine() != Some(Engine::SqlServer) {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Index maintenance requires SQL Server",
        )
        .into());
    }
    if store
        .current_connection_policy(session, source)?
        .is_some_and(|p| p.allowed_schemas.is_some())
    {
        return Err(ApiError::Forbidden(
            "Index maintenance requires an unrestricted schema profile".into(),
        ));
    }
    store.authorize_connection_operation(
        session,
        source,
        OperationKind::ExecuteQuery,
        Some(&sql),
        &[],
    )?;
    store.validate_execute_tx(session, source, None)?;
    let source_actor = actor(store, session, source).await?;
    let sql_for_work = sql.clone();
    store
        .with_sqlserver_owned_connection(
            session,
            source,
            OperationKind::MaintainSqlServerIndex,
            move |store, session, dedicated| async move {
                let work = async {
                    query(&store, session, dedicated, SETUP, vec![]).await?;
                    if actor(&store, session, dedicated).await? != source_actor {
                        return Err(bad("Source actor/database cannot be replayed exactly; reconnect without impersonation or USE before maintenance"));
                    }
                    let response = query(
                        &store,
                        session,
                        dedicated,
                        include_str!("sqlserver_index_state.sql"),
                        vec![
                            Value::Text(request.schema.clone()),
                            Value::Text(request.table.clone()),
                            Value::Text(request.index.clone()),
                        ],
                    )
                    .await?;
                    let row = response.rows.first()
                        .ok_or_else(|| bad("Index review is unavailable"))?;
                    let [Value::Bool(authority), Value::Bool(visible), Value::Bool(supported),
                        Value::Bool(pages), Value::Text(database), Value::Text(identity)] =
                        row.values.as_slice()
                    else {
                        return Err(bad("Invalid native index review"));
                    };
                    if !authority || !visible {
                        return Err(ApiError::Forbidden("Native ALTER and VIEW DEFINITION on the target table are required".into()));
                    }
                    if !supported || (request.action == SqlServerIndexAction::Reorganize && !pages) {
                        return Err(bad("Select an enabled ordinary single-partition disk rowstore index in a writable user database; REORGANIZE requires page locks. Unsupported native shapes are refused"));
                    }
                    if identity.len() != 64 {
                        return Err(bad("Invalid native index identity"));
                    }
                    let mut hash = Sha256::new();
                    hash.update(serde_json::to_vec(&(
                        session.0, source.0, &sql_for_work, &source_actor, identity,
                    )).map_err(|_| bad("Cannot encode index review"))?);
                    let precondition = format!("{:x}", hash.finalize());
                    if request.apply {
                        if request.precondition.as_deref() != Some(&precondition) {
                            return Err(bad("Native index/actor changed since preview; review again"));
                        }
                        store.authorize_connection_operation(
                            session, source, OperationKind::MaintainSqlServerIndex,
                            Some(&sql_for_work), &[],
                        )?;
                        store.authorize_connection_operation(
                            session, source, OperationKind::ExecuteQuery,
                            Some(&sql_for_work), &[],
                        )?;
                        if actor(&store, session, source).await? != source_actor {
                            return Err(bad("Source actor changed during review"));
                        }
                        query(&store, session, dedicated, &sql_for_work, vec![]).await?;
                    }
                    Ok(SqlServerIndexReport {
                        sql: sql_for_work,
                        applied: request.apply,
                        database: database.clone(),
                        precondition,
                        warning: "One explicit enabled rowstore index only. Offline REBUILD can block access and consume log/temp/disk space; UPDATE STATISTICS uses FULLSCAN and may change plans. REORGANIZE can retain partial work after cancellation. Timeout, failure or disconnect cannot establish completion/rollback; inspect the target before retrying. Preview is not a catalog lock. Required SET options, ROWCOUNT 0 and LOCK_TIMEOUT 5000 are normalized only on an owned connection; the editor is preserved. Verify a current backup. No performance improvement is guaranteed.".into(),
                    })
                };
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(DriverError::new(
                        Code::QueryCanceled,
                        "Index maintenance canceled; work may be partially applied and cleanup may remain pending",
                    ).into()),
                    result = work => result,
                }
            },
        )
        .await
}
