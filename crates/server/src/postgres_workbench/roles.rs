//! Public role lifecycle; secret-bearing pg_authid columns are never read.
use super::*;
use chrono::Datelike as _;
use sift_protocol::{
    PostgresObjectImpact, PostgresRoleAttributePatch as Patch, PostgresRoleValidity,
};

#[derive(serde::Deserialize)]
struct PublicRole {
    oid: String,
    rolname: String,
    rolsuper: bool,
    rolinherit: bool,
    rolcreaterole: bool,
    rolcreatedb: bool,
    rolcanlogin: bool,
    rolreplication: bool,
    rolbypassrls: bool,
    rolconnlimit: i32,
    valid_until: Option<String>,
}
#[derive(serde::Deserialize)]
struct State {
    version: u32,
    database: (String, String),
    actor: PublicRole,
    session_actor: PublicRole,
    target: Option<PublicRole>,
    destination: Option<PublicRole>,
    admin: bool,
    edges: Vec<serde_json::Value>,
    dependencies: Vec<serde_json::Value>,
}
fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn role_name(name: &str) -> ApiResult<&str> {
    let name = identifier(name)?;
    if name.to_ascii_lowercase().starts_with("pg_") || name.eq_ignore_ascii_case("public") {
        return Err(bad("Reserved PostgreSQL roles cannot be managed here"));
    }
    Ok(name)
}
pub(super) fn handles(action: &PostgresObjectAction) -> bool {
    matches!(
        action,
        PostgresObjectAction::CreateRole { .. }
            | PostgresObjectAction::CreateRoleWithAttributes { .. }
            | PostgresObjectAction::RenameRole { .. }
            | PostgresObjectAction::AlterRoleAttributes { .. }
            | PostgresObjectAction::DropRole { .. }
    )
}
fn options(patch: &Patch) -> ApiResult<Vec<String>> {
    let mut options = Vec::new();
    for (value, on, off) in [
        (patch.login, "LOGIN", "NOLOGIN"),
        (patch.inherit, "INHERIT", "NOINHERIT"),
        (patch.create_database, "CREATEDB", "NOCREATEDB"),
        (patch.create_role, "CREATEROLE", "NOCREATEROLE"),
        (patch.superuser, "SUPERUSER", "NOSUPERUSER"),
        (patch.replication, "REPLICATION", "NOREPLICATION"),
        (patch.bypass_rls, "BYPASSRLS", "NOBYPASSRLS"),
    ] {
        if let Some(value) = value {
            options.push(if value { on } else { off }.into());
        }
    }
    if let Some(limit) = patch.connection_limit {
        if limit < -1 {
            return Err(bad("Connection limit must be -1 or a nonnegative integer"));
        }
        options.push(format!("CONNECTION LIMIT {limit}"));
    }
    if let Some(validity) = patch.valid_until {
        let value = match validity {
            PostgresRoleValidity::Infinite => "infinity".into(),
            PostgresRoleValidity::Until { at } => {
                if !(1..=9999).contains(&at.year()) {
                    return Err(bad("Role expiry must have a UTC year from 1 through 9999"));
                }
                at.to_rfc3339()
            }
        };
        options.push(format!("VALID UNTIL '{value}'"));
    }
    Ok(options)
}
fn changed(patch: &Patch, role: &PublicRole) -> bool {
    [
        (patch.login, role.rolcanlogin),
        (patch.inherit, role.rolinherit),
        (patch.create_database, role.rolcreatedb),
        (patch.create_role, role.rolcreaterole),
        (patch.superuser, role.rolsuper),
        (patch.replication, role.rolreplication),
        (patch.bypass_rls, role.rolbypassrls),
    ]
    .iter()
    .any(|(p, current)| p.is_some_and(|p| p != *current))
        || patch
            .connection_limit
            .is_some_and(|p| p != role.rolconnlimit)
        || patch.valid_until.is_some_and(|p| match p {
            PostgresRoleValidity::Infinite => role.valid_until.as_deref() != Some("infinity"),
            PostgresRoleValidity::Until { at } => {
                role.valid_until
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .map(|v| v.with_timezone(&chrono::Utc))
                    != Some(at)
            }
        })
}
fn attribute_authority(patch: &Patch, state: &State, create: bool) -> ApiResult<()> {
    if state.actor.rolsuper {
        return Ok(());
    }
    if patch.superuser.is_some_and(|flag| !create || flag) {
        return Err(ApiError::Forbidden(
            "Changing SUPERUSER requires native superuser authority".into(),
        ));
    }
    for (requested, has) in [
        (patch.create_database, state.actor.rolcreatedb),
        (patch.replication, state.actor.rolreplication),
        (patch.bypass_rls, state.actor.rolbypassrls),
    ] {
        if requested.is_some_and(|flag| !create || flag) && !has {
            return Err(ApiError::Forbidden(
                "The actor lacks the requested native CREATEDB, REPLICATION or BYPASSRLS authority"
                    .into(),
            ));
        }
    }
    Ok(())
}
pub(super) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    let (name, new_name, patch, create) = match &action {
        PostgresObjectAction::CreateRole { name } => (name, None, None, true),
        PostgresObjectAction::CreateRoleWithAttributes { name, attributes } => {
            (name, None, Some(attributes), true)
        }
        PostgresObjectAction::AlterRoleAttributes { name, attributes } => {
            (name, None, Some(attributes), false)
        }
        PostgresObjectAction::RenameRole { name, new_name } => (name, Some(new_name), None, false),
        PostgresObjectAction::DropRole { name } => (name, None, None, false),
        _ => unreachable!("role lifecycle actions only"),
    };
    let name = role_name(name)?;
    let destination = new_name.map(|name| role_name(name)).transpose()?;
    if destination == Some(name) {
        return Err(bad("The role already has that name"));
    }
    let rendered = patch.map(options).transpose()?.unwrap_or_default();
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: include_str!("role-state.sql").into(),
                params: vec![
                    Value::Text(name.into()),
                    Value::Text(destination.unwrap_or("").into()),
                ],
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
    if json.len() > 1024 * 1024 {
        return Err(bad("Role lifecycle snapshot exceeds 1 MiB"));
    }
    let state: State = serde_json::from_str(json).map_err(|_| invalid_shape())?;
    if !matches!(action, PostgresObjectAction::CreateRole { .. }) && state.version < 160000 {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Role lifecycle and attribute patches require PostgreSQL 16 or newer",
        )
        .into());
    }
    if state.database.0 == "0" || state.database.1.is_empty() {
        return Err(invalid_shape());
    }
    if state.edges.len() > 200 || state.dependencies.len() > 200 {
        return Err(bad(
            "Role review exceeds 200 memberships or shared dependency addresses",
        ));
    }
    if !state.actor.rolsuper && !state.actor.rolcreaterole {
        return Err(ApiError::Forbidden(
            "Native CREATEROLE or superuser authority is required".into(),
        ));
    }
    let sql = if create {
        if state.target.is_some() {
            return Err(bad("The role already exists"));
        }
        if let Some(patch) = patch {
            attribute_authority(patch, &state, true)?;
        }
        format!(
            "CREATE ROLE {} {}",
            quote(name),
            if rendered.is_empty() {
                "NOLOGIN".into()
            } else {
                rendered.join(" ")
            }
        )
    } else {
        let target = state
            .target
            .as_ref()
            .ok_or_else(|| bad("The role was not found"))?;
        if !state.actor.rolsuper && (target.rolsuper || !state.admin) {
            return Err(ApiError::Forbidden(
                "Native CREATEROLE plus ADMIN authority on this non-superuser role is required"
                    .into(),
            ));
        }
        if let Some(patch) = patch {
            if target.oid == "10" && patch.superuser.is_some_and(|flag| flag != target.rolsuper) {
                return Err(bad(
                    "The bootstrap superuser's SUPERUSER property is immutable",
                ));
            }
            if !state.actor.rolsuper && target.rolreplication {
                return Err(ApiError::Forbidden(
                    "Changing attributes of a replication role requires native superuser authority"
                        .into(),
                ));
            }
            attribute_authority(patch, &state, false)?;
            if rendered.is_empty() || !changed(patch, target) {
                return Err(bad("Choose at least one changed public role attribute"));
            }
            format!("ALTER ROLE {} {}", quote(name), rendered.join(" "))
        } else {
            if target.oid == state.actor.oid || target.oid == state.session_actor.oid {
                return Err(bad(
                    "Rename or drop the current/session actor using a different connection",
                ));
            }
            if let Some(destination) = destination {
                if state.destination.is_some() {
                    return Err(bad("The new role name already exists"));
                }
                format!(
                    "ALTER ROLE {} RENAME TO {}",
                    quote(name),
                    quote(destination)
                )
            } else {
                format!("DROP ROLE {}", quote(name))
            }
        }
    };
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&(session.0, connection.0, &sql, json)).map_err(|_| invalid_shape())?,
    );
    let mut objects = vec![format!(
        "{} relevant explicit memberships; {} shared dependency addresses",
        state.edges.len(),
        state.dependencies.len()
    )];
    if let Some(role) = &state.target {
        objects.push(format!("Current {} (OID {}): LOGIN {}; INHERIT {}; CREATEDB {}; CREATEROLE {}; SUPERUSER {}; REPLICATION {}; BYPASSRLS {}; connection limit {}; password validity {}",role.rolname,role.oid,role.rolcanlogin,role.rolinherit,role.rolcreatedb,role.rolcreaterole,role.rolsuper,role.rolreplication,role.rolbypassrls,role.rolconnlimit,role.valid_until.as_deref().unwrap_or("unlimited")));
    }
    for dependency in &state.dependencies {
        objects.push(format!(
            "Shared dependency: database {} · class {} · object {} · subobject {} · kind {}",
            dependency["dbid"],
            dependency["classid"],
            dependency["objid"],
            dependency["objsubid"],
            dependency["deptype"]
        ));
    }
    let effect=match &action {
        PostgresObjectAction::RenameRole {..}=>"Renaming preserves role identity and existing memberships, but clears any MD5 password. Password hashes are not inspected.",
        PostgresObjectAction::DropRole {..}=>"DROP removes memberships and fails while native ownership/grant dependencies remain. It does not reassign ownership or delete owned objects.",
        _=>"Public attributes change authentication/authorization. INHERIT controls defaults for new memberships; existing membership options remain unchanged. LOGIN and connection limits do not terminate existing sessions. Password validity does not disable every authentication method.",
    };
    let warning=format!("Roles affect the whole PostgreSQL cluster. {effect} This bounded review is not a full effective-privilege analysis; native checks may still reject apply.");
    Ok(PostgresObjectPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning,
        impact: Some(PostgresObjectImpact {
            objects,
            truncated: false,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_role_patches_refuse_secret_fields_and_invalid_native_limits() {
        assert!(
            serde_json::from_value::<Patch>(serde_json::json!({"password":"private"})).is_err()
        );
        assert!(options(&Patch {
            connection_limit: Some(-2),
            ..Default::default()
        })
        .is_err());
        assert!(role_name("pg_database_owner").is_err());
        assert!(role_name("PUBLIC").is_err());
        assert!(role_name("é".repeat(32).as_str()).is_err());
        let sql = options(&Patch {
            login: Some(false),
            inherit: Some(true),
            connection_limit: Some(0),
            valid_until: Some(PostgresRoleValidity::Infinite),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            sql,
            [
                "NOLOGIN",
                "INHERIT",
                "CONNECTION LIMIT 0",
                "VALID UNTIL 'infinity'"
            ]
        );
    }
}
