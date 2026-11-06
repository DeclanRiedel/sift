//! Native effective ACL checks, without impersonation or reading protected data.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sift_protocol::*;
const WARNING: &str = "Native ACL checks for the selected role, including PUBLIC and effective inherited membership. Only objects visible to the connected actor are listed; omitted objects are not denials. Schema USAGE, row security, view/security-definer behavior, dependencies and login rules still affect actual access. RLS flags are not a policy evaluation. This report neither executes as the selected role nor changes grants. System/TEMP and additional privilege families are excluded. Pages observe a changing catalog, not a stable snapshot; Native ACL provenance distinguishes stored grants from built-in defaults, and is bounded to 64 grant rows per object (at most 65 native ACL items expanded). Truncation is explicit. Subject-grantee access uses native role USAGE, including superuser semantics; it is not a membership path. Column ACLs exclude table grants, and owner inherent grant options are not ACL rows. No grant-chain or inheritance path is inferred.";
pub async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresPrivilegeQuery,
) -> ApiResult<PostgresPrivilegeReport> {
    let limit = query.limit.unwrap_or(100);
    if !(1..=200).contains(&limit)
        || query.offset > 10_000
        || query.role.as_ref().is_some_and(|role| {
            role.is_empty() || role.len() > 63 || role.chars().any(char::is_control)
        })
    {
        return Err(ApiError::BadRequest(
            "Use a valid role name, limit 1..200 and offset <=10000".into(),
        ));
    }
    let entry = store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ReadPostgresPrivileges,
        None,
        &[],
    )?;
    if entry.driver.semantic_engine() != Some(Engine::Postgres) {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Privilege matrix requires PostgreSQL",
        )
        .into());
    }
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: include_str!("postgres_privileges/state.sql").into(),
                params: vec![
                    query.role.map(Value::Text).unwrap_or(Value::Null),
                    Value::Int64(i64::from(limit + 1)),
                    Value::Int64(i64::from(query.offset)),
                ],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ReadPostgresPrivileges,
        )
        .await?;
    let row = response
        .rows
        .first()
        .ok_or_else(|| ApiError::BadRequest("Selected role is unavailable".into()))?;
    let [Value::Text(json)] = row.values.as_slice() else {
        return Err(ApiError::Internal("Invalid native privilege matrix".into()));
    };
    if json.len() > 1_048_576 {
        return Err(ApiError::BadRequest(
            "Native privilege page exceeds 1 MiB".into(),
        ));
    }
    let mut report: PostgresPrivilegeReport = serde_json::from_str(json)
        .map_err(|_| ApiError::Internal("Invalid native privilege matrix".into()))?;
    report.page_limit_reached =
        report.objects.len() > limit as usize && query.offset + limit > 10_000;
    report.next_offset = (report.objects.len() > limit as usize && query.offset + limit <= 10_000)
        .then_some(query.offset + limit);
    report.objects.truncate(limit as usize);
    report.warning = WARNING.into();
    // A policy revoked during the native read must not publish catalog results.
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ReadPostgresPrivileges,
        None,
        &[],
    )?;
    Ok(report)
}
