//! Native ownership reviews disclose permission loss and bind catalog identity.
use super::*;
use sift_protocol::SqlServerOwnedObjectKind as Kind;
const SCHEMA: &str = include_str!("ownership-schema.sql");
const DATABASE: &str = include_str!("ownership-database.sql");

pub(super) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: SqlServerSecurityAction,
) -> ApiResult<SqlServerSecurityPreview> {
    let SqlServerSecurityAction::ChangeOwner {
        object_kind,
        name,
        new_owner,
    } = &action
    else {
        unreachable!("ownership action");
    };
    let name = identifier(name)?;
    let new_owner = identifier(new_owner)?;
    let (lookup, class) = match object_kind {
        Kind::Schema => (SCHEMA, "SCHEMA"),
        Kind::Database => (DATABASE, "DATABASE"),
    };
    let response = run(
        store,
        session,
        connection,
        lookup,
        vec![Value::Text(name.into()), Value::Text(new_owner.into())],
        OperationKind::PreviewSqlServerSecurity,
    )
    .await?;
    let state = response.rows.first().map(|row| row.values.as_slice()).ok_or_else(|| ApiError::BadRequest("Select an existing supported owner and an unprotected schema or the current user database".into()))?;
    let [Value::Bool(same), Value::Bool(take), Value::Bool(target), Value::Bool(visible), Value::Text(_), Value::Int64(schema_grants), Value::Int64(inherited_grants)] =
        state
    else {
        return Err(shape_error());
    };
    if !take || !target {
        return Err(ApiError::Forbidden(
            "Ownership transfer requires TAKE OWNERSHIP and authority for the selected owner"
                .into(),
        ));
    }
    if !visible {
        return Err(ApiError::Forbidden(
            "Complete schema ownership review requires database VIEW DEFINITION/security metadata without effective metadata DENYs"
                .into(),
        ));
    }
    if *same {
        return Err(ApiError::BadRequest(
            "The selected principal already owns this object".into(),
        ));
    }
    let sql = format!(
        "ALTER AUTHORIZATION ON {class}::{} TO {}",
        quote(name),
        quote(new_owner)
    );
    let mut digest = Sha256::new();
    digest.update(format!("{session}:{connection}").as_bytes());
    digest.update(sql.as_bytes());
    digest.update(serde_json::to_vec(state).map_err(|_| shape_error())?);
    let warning = match object_kind {
        Kind::Schema => format!("Schema transfer removes {schema_grants} explicit schema permission rows and {inherited_grants} permission rows on schema-contained objects, types and XML schema collections that inherit their owner. Explicitly owned objects retain their own permissions. Ownership chains and EXECUTE AS OWNER behavior can change. No permissions are regranted. Preview holds no locks; native SQL Server validates final authority."),
        Kind::Database => "Database ownership transfers full administrative control to the selected existing server login. Ownership chains and EXECUTE AS OWNER behavior can change. TRUSTWORTHY and cross-database chaining settings are not changed. Preview holds no locks; native SQL Server validates final authority and owner mapping.".into(),
    };
    Ok(SqlServerSecurityPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning,
    })
}
