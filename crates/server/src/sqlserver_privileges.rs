//! Native effective checks on a disposable, supervised SQL Server connection.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sift_protocol::*;
const WARNING:&str="Native effective permission checks on a fresh authenticated connection. A selected database-user token excludes login/server permissions and does not prove login access. Only represented database/schema/ordinary table/view/column/sequence/SQL-routine families are inspected; omitted objects are not denials. NULL means unavailable. Checks include native inheritance, ownership, fixed roles and DENY/column exceptions, not inferred ACL paths or grant options. Protected data and routines are not executed. Ownership chains, module execution context, row security and external authorization can affect actual access. Editor transaction/session tokens are not inherited. Pages observe changing catalogs and permissions, not a stable cross-page snapshot.";
pub async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: SqlServerPrivilegeQuery,
) -> ApiResult<SqlServerPrivilegeReport> {
    let limit = query.limit.unwrap_or(100);
    if !(1..=100).contains(&limit)
        || query.offset > 10000
        || query.principal.as_ref().is_some_and(|name| {
            name.is_empty()
                || name.encode_utf16().count() > 128
                || name.chars().any(char::is_control)
        })
    {
        return Err(ApiError::BadRequest(
            "Use a principal name of 1..128 UTF-16 units, limit 1..100 and offset <=10000".into(),
        ));
    }
    if store.conn_entry(session, connection)?.driver.engine() != Engine::SqlServer {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Effective-permission matrix requires SQL Server",
        )
        .into());
    }
    let report = store
        .with_sqlserver_inspection_connection(
            session,
            connection,
            OperationKind::ReadSqlServerPrivileges,
            move |store, session, connection| async move {
                read_owned(&store, session, connection, query, limit).await
            },
        )
        .await?;
    // Cleanup may wait after the owned helper's source recheck. Do not publish
    // catalog data after a source close or policy revocation during that wait.
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ReadSqlServerPrivileges,
        None,
        &[],
    )?;
    Ok(report)
}
async fn read_owned(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: SqlServerPrivilegeQuery,
    limit: u32,
) -> ApiResult<SqlServerPrivilegeReport> {
    let result = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: include_str!("sqlserver_privileges/state.sql").into(),
                params: vec![
                    query
                        .principal
                        .map(Value::Text)
                        .unwrap_or_else(|| Value::TypedNull {
                            type_name: "nvarchar(128)".into(),
                        }),
                    Value::Int64(i64::from(query.offset)),
                    Value::Int64(i64::from(limit + 1)),
                ],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ReadSqlServerPrivileges,
        )
        .await;
    let response = match result {
        Err(ApiError::Driver(error))
            if matches!(
                error.native_code.as_deref(),
                Some("50001" | "15517" | "229" | "297")
            ) =>
        {
            return Err(ApiError::Forbidden(
                "Native metadata or impersonation authority is insufficient for this subject"
                    .into(),
            ))
        }
        Err(ApiError::Driver(error)) if error.native_code.as_deref() == Some("50002") => {
            return Err(ApiError::BadRequest(
                "Select the fresh connected actor or an ordinary SQL database user".into(),
            ))
        }
        Err(ApiError::Driver(error)) if error.native_code.as_deref() == Some("50003") => {
            return Err(ApiError::BadRequest(
                "Native identity changed during inspection; reload the matrix".into(),
            ))
        }
        other => other?,
    };
    let [row] = response.rows.as_slice() else {
        return Err(shape_error());
    };
    let [Value::Text(json)] = row.values.as_slice() else {
        return Err(shape_error());
    };
    if json.len() > 1_048_576 {
        return Err(ApiError::BadRequest(
            "Native effective-permission page exceeds 1 MiB".into(),
        ));
    }
    let mut report: SqlServerPrivilegeReport =
        serde_json::from_str(json).map_err(|_| shape_error())?;
    if report.objects.len() > limit as usize + 1
        || report.objects.iter().any(|object| object.checks.len() > 20)
    {
        return Err(shape_error());
    }
    report.page_limit_reached =
        report.objects.len() > limit as usize && query.offset + limit > 10000;
    report.next_offset = (report.objects.len() > limit as usize && query.offset + limit <= 10000)
        .then_some(query.offset + limit);
    report.objects.truncate(limit as usize);
    report.warning = WARNING.into();
    Ok(report)
}

fn shape_error() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "Unexpected native effective-permission report",
    )
    .into()
}
