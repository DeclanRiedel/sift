//! SQL-authenticated database users; credentials never enter these actions.
use super::*;
const STATE: &str = include_str!("review-user.sql");

pub(super) fn is_user_action(action: &SqlServerSecurityAction) -> bool {
    matches!(
        action,
        SqlServerSecurityAction::CreateDatabaseUser { .. }
            | SqlServerSecurityAction::RemapDatabaseUser { .. }
            | SqlServerSecurityAction::SetUserDefaultSchema { .. }
            | SqlServerSecurityAction::DropDatabaseUser { .. }
    )
}

pub(super) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: SqlServerSecurityAction,
) -> ApiResult<SqlServerSecurityPreview> {
    use SqlServerSecurityAction::*;
    let (name, secondary, mode, sql) = match &action {
        CreateDatabaseUser { name, login } => {
            let name = identifier(name)?;
            if name.contains(char::from(92)) {
                return Err(ApiError::BadRequest(
                    "SQL login-mapped user names cannot contain a backslash".into(),
                ));
            }
            let login = identifier(login)?;
            (
                name,
                login,
                "create",
                format!(
                    "CREATE USER {} FOR LOGIN {} WITH DEFAULT_SCHEMA = [dbo]",
                    quote(name),
                    quote(login)
                ),
            )
        }
        RemapDatabaseUser { name, login } => {
            let name = identifier(name)?;
            let login = identifier(login)?;
            (
                name,
                login,
                "remap",
                format!(
                    "ALTER USER {} WITH LOGIN = {}, NAME = {}",
                    quote(name),
                    quote(login),
                    quote(name)
                ),
            )
        }
        SetUserDefaultSchema { name, schema } => {
            let name = identifier(name)?;
            let schema = identifier(schema)?;
            (
                name,
                schema,
                "schema",
                format!(
                    "ALTER USER {} WITH DEFAULT_SCHEMA = {}",
                    quote(name),
                    quote(schema)
                ),
            )
        }
        DropDatabaseUser { name } => {
            let name = identifier(name)?;
            (name, "", "drop", format!("DROP USER {}", quote(name)))
        }
        _ => unreachable!("user action routing"),
    };
    let response = run(
        store,
        session,
        connection,
        STATE,
        vec![
            Value::Text(name.into()),
            Value::Text(secondary.into()),
            Value::Text(mode.into()),
        ],
        OperationKind::PreviewSqlServerSecurity,
    )
    .await?;
    let state = response
        .rows
        .first()
        .map(|row| row.values.as_slice())
        .ok_or_else(shape_error)?;
    let [Value::Bool(supported), Value::Bool(authority), Value::Bool(visible), Value::Bool(collision), Value::Bool(same), Value::Int64(owned), Value::Int64(memberships), Value::Int64(permissions), Value::Bool(disabled), Value::Text(identity)] =
        state
    else {
        return Err(shape_error());
    };
    if identity.len() != 64
        || !identity.bytes().all(|byte| byte.is_ascii_hexdigit())
        || *owned < 0
        || *memberships < 0
        || *permissions < 0
    {
        return Err(shape_error());
    }
    if !authority {
        return Err(ApiError::Forbidden(
            match mode {
                "create" => "CREATE USER or ALTER ANY USER authority is required",
                "remap" => "Database CONTROL authority is required to remap a user",
                "schema" => "ALTER-on-user authority or the user's own context is required",
                _ => "ALTER ANY USER authority is required to drop a user",
            }
            .into(),
        ));
    }
    if !visible {
        return Err(ApiError::Forbidden("Complete user review requires database VIEW DEFINITION/security metadata without effective metadata DENYs".into()));
    }
    if !supported {
        return Err(ApiError::BadRequest("Select an unprotected supported SQL database user, an existing visible SQL login/schema, or a new user name; WITHOUT LOGIN users cannot be remapped, and the current actor cannot be remapped or dropped".into()));
    }
    if *collision {
        return Err(ApiError::BadRequest(
            "The login is already mapped to another database principal or owns this database"
                .into(),
        ));
    }
    if *same {
        return Err(ApiError::BadRequest(
            "User mapping or default schema is already in the requested state".into(),
        ));
    }
    if mode == "drop" && *owned > 0 {
        return Err(ApiError::BadRequest("Transfer the user's owned securables before dropping it; no ownership is transferred automatically".into()));
    }
    let mut warning=match mode {
        "create" => "Creates a database user for the existing SQL login with default schema dbo. The login is not created, enabled or changed. Native SQL Server validates mapping and final authority.".to_string(),
        "remap" => format!("Remaps the user's SID to the selected SQL login, transferring its existing direct/inherited database access ({memberships} direct role memberships and {permissions} explicit permission rows involving this principal). Name, principal ID, default schema, ownership and existing rights are preserved. Existing sessions can retain old tokens. Effective/fixed-role privileges are not expanded here; concurrent inherited-right changes remain possible. The old login is not deleted and the target login is not enabled or changed."),
        "schema" => "Changes unqualified-name resolution and the default target for newly created objects. No objects or schema ownership are moved. A sysadmin login still uses dbo. Preview holds no locks; native SQL Server validates final authority.".to_string(),
        _ => format!("Drops only the database user; the server login survives. Review includes {memberships} direct role memberships and {permissions} explicit permission rows involving the principal; native DROP USER determines their removal or refusal. No objects are dropped or ownership transferred. Remaining ownership families and final authority are validated by SQL Server. Preview holds no locks."),
    };
    if *disabled {
        warning.push_str(" The selected login is disabled and remains disabled.");
    }
    let mut digest = Sha256::new();
    digest.update(format!("{session}:{connection}").as_bytes());
    digest.update(sql.as_bytes());
    digest.update(serde_json::to_vec(state).map_err(|_| shape_error())?);
    Ok(SqlServerSecurityPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning,
    })
}
