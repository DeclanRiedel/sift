//! Bounded SQL Server security catalogs and guarded database-scoped changes.

use sha2::{Digest, Sha256};
use sift_protocol::{
    ApplySqlServerSecurityRequest, Code, ConnectionId, DriverError, Engine, ExecuteRequestHttp,
    OperationKind, SessionId, SqlServerLogin, SqlServerPrincipal, SqlServerRoleMembership,
    SqlServerSchemaOwner, SqlServerSchemaPermission, SqlServerSecurityAction,
    SqlServerSecurityPreview, SqlServerSecurityReport, SqlServerSecuritySection,
    SqlServerSecurityState, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const DATABASE_SQL: &str = "SELECT CONVERT(nvarchar(128), DB_NAME())";
const LOGINS_SQL: &str = "SELECT TOP (101) CONVERT(nvarchar(128), name), CONVERT(nvarchar(60), type_desc) FROM sys.server_principals WHERE type IN ('S','U','G') ORDER BY name";
const PRINCIPALS_SQL: &str = "SELECT TOP (101) CONVERT(nvarchar(128), name), CONVERT(nvarchar(60), type_desc), COALESCE(CONVERT(nvarchar(60), authentication_type_desc), N'') FROM sys.database_principals WHERE type IN ('S','U','G','R') ORDER BY name";
const MEMBERSHIPS_SQL: &str = "SELECT TOP (101) CONVERT(nvarchar(128), role.name), CONVERT(nvarchar(128), member.name) FROM sys.database_role_members AS membership JOIN sys.database_principals AS role ON role.principal_id = membership.role_principal_id JOIN sys.database_principals AS member ON member.principal_id = membership.member_principal_id ORDER BY role.name, member.name";
const SCHEMAS_SQL: &str = "SELECT TOP (101) CONVERT(nvarchar(128), schema_name.name), CONVERT(nvarchar(128), owner.name) FROM sys.schemas AS schema_name JOIN sys.database_principals AS owner ON owner.principal_id = schema_name.principal_id ORDER BY schema_name.name";
const PERMISSIONS_SQL: &str = "SELECT TOP (101) CONVERT(nvarchar(128), schema_name.name), CONVERT(nvarchar(128), grantee.name), CONVERT(nvarchar(128), permission.permission_name), CONVERT(nvarchar(60), permission.state_desc) FROM sys.database_permissions AS permission JOIN sys.schemas AS schema_name ON permission.class = 3 AND permission.major_id = schema_name.schema_id JOIN sys.database_principals AS grantee ON grantee.principal_id = permission.grantee_principal_id ORDER BY schema_name.name, grantee.name, permission.permission_name";

pub async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<SqlServerSecurityReport> {
    require_sqlserver(store, session, connection)?;
    let database = run(
        store,
        session,
        connection,
        DATABASE_SQL,
        Vec::new(),
        OperationKind::ReadSqlServerSecurity,
    )
    .await?
    .rows
    .first()
    .and_then(|row| row.values.first())
    .and_then(text)
    .ok_or_else(shape_error)?;
    Ok(SqlServerSecurityReport {
        database,
        logins: section(store, session, connection, LOGINS_SQL, |v| {
            if v.len() != 2 {
                return Err(shape_error());
            }
            Ok(SqlServerLogin {
                name: required(&v[0])?,
                kind: required(&v[1])?,
            })
        })
        .await?,
        principals: section(store, session, connection, PRINCIPALS_SQL, |v| {
            if v.len() != 3 {
                return Err(shape_error());
            }
            Ok(SqlServerPrincipal {
                name: required(&v[0])?,
                kind: required(&v[1])?,
                authentication: required(&v[2])?,
            })
        })
        .await?,
        memberships: section(store, session, connection, MEMBERSHIPS_SQL, |v| {
            if v.len() != 2 {
                return Err(shape_error());
            }
            Ok(SqlServerRoleMembership {
                role: required(&v[0])?,
                member: required(&v[1])?,
            })
        })
        .await?,
        schemas: section(store, session, connection, SCHEMAS_SQL, |v| {
            if v.len() != 2 {
                return Err(shape_error());
            }
            Ok(SqlServerSchemaOwner {
                schema: required(&v[0])?,
                owner: required(&v[1])?,
            })
        })
        .await?,
        schema_permissions: section(store, session, connection, PERMISSIONS_SQL, |v| {
            if v.len() != 4 {
                return Err(shape_error());
            }
            Ok(SqlServerSchemaPermission {
                schema: required(&v[0])?,
                grantee: required(&v[1])?,
                permission: required(&v[2])?,
                state: required(&v[3])?,
            })
        })
        .await?,
    })
}

async fn section<T>(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: &str,
    parse: impl Fn(&[Value]) -> ApiResult<T>,
) -> ApiResult<SqlServerSecuritySection<T>> {
    let response = run(
        store,
        session,
        connection,
        sql,
        Vec::new(),
        OperationKind::ReadSqlServerSecurity,
    )
    .await;
    let response = match response {
        Err(error) if permission_denied(&error) => {
            return Ok(SqlServerSecuritySection {
                state: SqlServerSecurityState::PermissionRequired,
                items: Vec::new(),
                truncated: false,
            })
        }
        other => other?,
    };
    let truncated = response.rows.len() > 100;
    let items = response
        .rows
        .iter()
        .take(100)
        .map(|row| parse(&row.values))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(SqlServerSecuritySection {
        state: SqlServerSecurityState::Available,
        items,
        truncated,
    })
}

fn permission_denied(error: &ApiError) -> bool {
    matches!(error, ApiError::Driver(driver) if matches!(driver.native_code.as_deref(), Some("229" | "297" | "916")))
}

fn require_sqlserver(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<()> {
    if store.conn_entry(session, connection)?.driver.engine() != Engine::SqlServer {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "SQL Server security workbench requires SQL Server",
        )
        .into());
    }
    Ok(())
}

async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: &str,
    params: Vec<Value>,
    operation: OperationKind,
) -> ApiResult<sift_protocol::ExecuteResponse> {
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
            operation,
        )
        .await
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Native { display_text, .. } => Some(display_text.clone()),
        _ => None,
    }
}
fn required(value: &Value) -> ApiResult<String> {
    text(value).ok_or_else(shape_error)
}
fn boolean(value: &Value) -> ApiResult<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(shape_error()),
    }
}
fn shape_error() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "SQL Server security catalog returned an unexpected shape",
    )
    .into()
}

fn identifier(value: &str) -> ApiResult<&str> {
    if value.is_empty() || value.chars().count() > 128 || value.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "SQL Server identifier must have 1..=128 characters and no controls".into(),
        ));
    }
    Ok(value)
}
fn quote(value: &str) -> String {
    format!("[{}]", value.replace(']', "]]"))
}

const ROLE_STATE: &str = "SELECT CONVERT(bit, CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name = @P1) THEN 1 ELSE 0 END), CONVERT(bit, CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE ROLE') = 1 OR IS_MEMBER('db_securityadmin') = 1 THEN 1 ELSE 0 END)";
const MEMBERSHIP_STATE: &str = "SELECT CONVERT(bit, CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name = @P1 AND type = 'R' AND is_fixed_role = 0) THEN 1 ELSE 0 END), CONVERT(bit, CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name = @P2) THEN 1 ELSE 0 END), CONVERT(bit, CASE WHEN EXISTS(SELECT 1 FROM sys.database_role_members AS rm JOIN sys.database_principals AS r ON r.principal_id = rm.role_principal_id JOIN sys.database_principals AS m ON m.principal_id = rm.member_principal_id WHERE r.name = @P1 AND m.name = @P2) THEN 1 ELSE 0 END), CONVERT(bit, CASE WHEN HAS_PERMS_BY_NAME(@P1, 'ROLE', 'ALTER') = 1 OR HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'ALTER ANY ROLE') = 1 OR IS_MEMBER('db_securityadmin') = 1 THEN 1 ELSE 0 END)";
const SCHEMA_STATE: &str = "SELECT CONVERT(bit, CASE WHEN EXISTS(SELECT 1 FROM sys.schemas WHERE name = @P1) THEN 1 ELSE 0 END), CONVERT(bit, CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name = @P2) THEN 1 ELSE 0 END), COALESCE((SELECT TOP (1) CONVERT(nvarchar(60), permission.state_desc) FROM sys.database_permissions AS permission JOIN sys.schemas AS schema_name ON schema_name.schema_id = permission.major_id JOIN sys.database_principals AS grantee ON grantee.principal_id = permission.grantee_principal_id WHERE permission.class = 3 AND schema_name.name = @P1 AND grantee.name = @P2 AND permission.permission_name = 'SELECT'), N''), CONVERT(bit, CASE WHEN HAS_PERMS_BY_NAME(@P1, 'SCHEMA', 'CONTROL') = 1 THEN 1 ELSE 0 END)";

pub async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: SqlServerSecurityAction,
) -> ApiResult<SqlServerSecurityPreview> {
    require_sqlserver(store, session, connection)?;
    let (lookup, params, sql, warning) = match &action {
        SqlServerSecurityAction::CreateDatabaseRole { name } => {
            let name = identifier(name)?;
            (
                ROLE_STATE,
                vec![Value::Text(name.into())],
                format!("CREATE ROLE {}", quote(name)),
                "Creating a role changes database authorization".to_string(),
            )
        }
        SqlServerSecurityAction::AddRoleMember { role, member }
        | SqlServerSecurityAction::DropRoleMember { role, member } => {
            let role = identifier(role)?;
            let member = identifier(member)?;
            if role == member {
                return Err(ApiError::BadRequest(
                    "A role cannot be its own member".into(),
                ));
            }
            let verb = if matches!(action, SqlServerSecurityAction::AddRoleMember { .. }) {
                "ADD"
            } else {
                "DROP"
            };
            (
                MEMBERSHIP_STATE,
                vec![Value::Text(role.into()), Value::Text(member.into())],
                format!("ALTER ROLE {} {verb} MEMBER {}", quote(role), quote(member)),
                "Changing role membership changes inherited database access".to_string(),
            )
        }
        SqlServerSecurityAction::GrantSchemaSelect { schema, grantee }
        | SqlServerSecurityAction::RevokeSchemaSelect { schema, grantee } => {
            let schema = identifier(schema)?;
            let grantee = identifier(grantee)?;
            let sql = if matches!(action, SqlServerSecurityAction::GrantSchemaSelect { .. }) {
                format!(
                    "GRANT SELECT ON SCHEMA::{} TO {}",
                    quote(schema),
                    quote(grantee)
                )
            } else {
                format!(
                    "REVOKE SELECT ON SCHEMA::{} FROM {}",
                    quote(schema),
                    quote(grantee)
                )
            };
            (
                SCHEMA_STATE,
                vec![Value::Text(schema.into()), Value::Text(grantee.into())],
                sql,
                "Changing a schema grant changes access for the principal and its members"
                    .to_string(),
            )
        }
    };
    let response = run(
        store,
        session,
        connection,
        lookup,
        params,
        OperationKind::PreviewSqlServerSecurity,
    )
    .await?;
    let state = response
        .rows
        .first()
        .map(|row| &row.values)
        .ok_or_else(shape_error)?;
    match &action {
        SqlServerSecurityAction::CreateDatabaseRole { .. } => {
            if state.len() != 2 {
                return Err(shape_error());
            }
            if !boolean(&state[1])? {
                return Err(ApiError::Forbidden(
                    "CREATE ROLE authority is required".into(),
                ));
            }
            if boolean(&state[0])? {
                return Err(ApiError::BadRequest(
                    "Database principal already exists".into(),
                ));
            }
        }
        SqlServerSecurityAction::AddRoleMember { .. }
        | SqlServerSecurityAction::DropRoleMember { .. } => {
            if state.len() != 4 {
                return Err(shape_error());
            }
            if !boolean(&state[3])? {
                return Err(ApiError::Forbidden(
                    "ALTER role authority is required".into(),
                ));
            }
            if !boolean(&state[0])? || !boolean(&state[1])? {
                return Err(ApiError::BadRequest("Role or member does not exist".into()));
            }
            if boolean(&state[2])?
                == matches!(action, SqlServerSecurityAction::AddRoleMember { .. })
            {
                return Err(ApiError::BadRequest(
                    "Role membership is already in the requested state".into(),
                ));
            }
        }
        SqlServerSecurityAction::GrantSchemaSelect { .. }
        | SqlServerSecurityAction::RevokeSchemaSelect { .. } => {
            if state.len() != 4 {
                return Err(shape_error());
            }
            if !boolean(&state[3])? {
                return Err(ApiError::Forbidden(
                    "Schema CONTROL authority is required".into(),
                ));
            }
            if !boolean(&state[0])? || !boolean(&state[1])? {
                return Err(ApiError::BadRequest(
                    "Schema or grantee does not exist".into(),
                ));
            }
            let existing = required(&state[2])?;
            let granted = existing == "GRANT" || existing == "GRANT_WITH_GRANT_OPTION";
            if granted == matches!(action, SqlServerSecurityAction::GrantSchemaSelect { .. }) {
                return Err(ApiError::BadRequest(
                    "Schema SELECT is already in the requested state".into(),
                ));
            }
            if !existing.is_empty() && !granted {
                return Err(ApiError::BadRequest(
                    "A DENY or unsupported permission state needs separate review".into(),
                ));
            }
        }
    }
    let state = serde_json::to_vec(state).map_err(|_| shape_error())?;
    let mut digest = Sha256::new();
    digest.update(format!("{session}:{connection}").as_bytes());
    digest.update(sql.as_bytes());
    digest.update(&state);
    Ok(SqlServerSecurityPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning,
    })
}

pub async fn apply(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: ApplySqlServerSecurityRequest,
) -> ApiResult<()> {
    if !request.production_confirmed || request.precondition.len() != 64 {
        return Err(ApiError::BadRequest(
            "A current preview and explicit production confirmation are required".into(),
        ));
    }
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ApplySqlServerSecurity,
        None,
        &[],
    )?;
    let fresh = preview(store, session, connection, request.action).await?;
    if fresh.precondition != request.precondition {
        return Err(ApiError::BadRequest(
            "SQL Server security state changed since preview".into(),
        ));
    }
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteQuery,
        Some(&fresh.sql),
        &[],
    )?;
    run(
        store,
        session,
        connection,
        &fresh.sql,
        Vec::new(),
        OperationKind::ApplySqlServerSecurity,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlserver_names_are_one_escaped_identifier() {
        assert_eq!(quote("a]; DROP ROLE x; --"), "[a]]; DROP ROLE x; --]");
        assert!(identifier("").is_err());
        assert!(identifier("bad\nname").is_err());
        assert!(identifier(&"x".repeat(129)).is_err());
    }
}
