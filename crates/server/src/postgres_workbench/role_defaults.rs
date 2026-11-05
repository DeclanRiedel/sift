//! Public workload defaults with native authority and optimistic identity review.
use super::*;
use sift_protocol::{PostgresObjectImpact, PostgresRoleDefaultScope as Scope};

#[derive(serde::Deserialize)]
struct Role {
    oid: String,
    rolname: String,
    rolsuper: bool,
    rolcreaterole: bool,
}
#[derive(serde::Deserialize)]
struct State {
    version: u32,
    database: (String, String),
    actor: Role,
    target: Option<Role>,
    admin: bool,
    global_value: Option<String>,
    database_value: Option<String>,
}
pub(super) fn handles(action: &PostgresObjectAction) -> bool {
    matches!(
        action,
        PostgresObjectAction::SetRoleDefault { .. } | PostgresObjectAction::ResetRoleDefault { .. }
    )
}
pub(super) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    let (name, scope, setting, value) = match &action {
        PostgresObjectAction::SetRoleDefault {
            name,
            scope,
            setting,
            value,
        } => (
            name,
            *scope,
            *setting,
            Some(
                setting
                    .native_value(value)
                    .map_err(|m| ApiError::BadRequest(m.into()))?,
            ),
        ),
        PostgresObjectAction::ResetRoleDefault {
            name,
            scope,
            setting,
        } => (name, *scope, *setting, None),
        _ => unreachable!("role default actions only"),
    };
    let name = roles::role_name(name)?;
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: include_str!("role-default-state.sql").into(),
                params: vec![Value::Text(name.into()), Value::Text(setting.name().into())],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::PreviewPostgresObject,
        )
        .await?;
    let row = response.rows.first().ok_or_else(invalid_shape)?;
    let [Value::Text(json)] = row.values.as_slice() else {
        return Err(invalid_shape());
    };
    if json.len() > 16_384 {
        return Err(invalid_shape());
    }
    let state: State = serde_json::from_str(json).map_err(|_| invalid_shape())?;
    if state.version < 160000 {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Role default review requires PostgreSQL 16 or newer",
        )
        .into());
    }
    let target = state
        .target
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("The role was not found".into()))?;
    if !state.actor.rolsuper
        && state.actor.oid != target.oid
        && (target.rolsuper || !state.actor.rolcreaterole || !state.admin)
    {
        return Err(ApiError::Forbidden("Native self authority, superuser, or CREATEROLE plus ADMIN on this non-superuser role is required".into()));
    }
    if state.database.0 == "0"
        || state.database.1.is_empty()
        || [&state.global_value, &state.database_value]
            .iter()
            .any(|v| v.as_ref().is_some_and(|v| v.len() > 64))
    {
        return Err(invalid_shape());
    }
    let current = match scope {
        Scope::AllDatabases => &state.global_value,
        Scope::CurrentDatabase => &state.database_value,
    };
    if current == &value {
        return Err(ApiError::BadRequest(
            "The selected role default already has that value or is already reset".into(),
        ));
    }
    let database = match scope {
        Scope::AllDatabases => String::new(),
        Scope::CurrentDatabase => format!(" IN DATABASE {}", quote(&state.database.1)),
    };
    let change = match value {
        Some(value) => format!("SET {} TO '{value}'", setting.name()),
        None => format!("RESET {}", setting.name()),
    };
    let sql = format!("ALTER ROLE {}{database} {change}", quote(name));
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&(session.0, connection.0, &sql, json)).map_err(|_| invalid_shape())?,
    );
    Ok(PostgresObjectPreview {
        action, sql, precondition:format!("{:x}",digest.finalize()),
        warning:"Role defaults affect new logins only; current sessions and SET ROLE retain their settings. Logical reconnect may reuse a pooled native session; a fresh native login is required. Database-specific role defaults override all-database role defaults. RESET falls back to other defaults. These are overrideable workload defaults, not security/resource-limit enforcement. Review is not a catalog lock; native checks remain authoritative.".into(),
        impact:Some(PostgresObjectImpact { objects:vec![format!("Role {} (OID {}), actor {} (OID {}), database {} (OID {})",target.rolname,target.oid,state.actor.rolname,state.actor.oid,state.database.1,state.database.0),format!("{} · {:?} · current selected value: {}",setting.name(),scope,current.as_deref().unwrap_or("not set")),format!("All-database value: {}; connected-database value: {}",state.global_value.as_deref().unwrap_or("not set"),state.database_value.as_deref().unwrap_or("not set"))], truncated:false }),
    })
}
