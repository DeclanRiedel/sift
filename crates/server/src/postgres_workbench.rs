//! Bounded PostgreSQL object and administration inspection with guarded edits.
use sha2::{Digest, Sha256};
use sift_protocol::{
    ApplyPostgresObjectRequest, Code, ConnectionId, DriverError, Engine, ExecuteRequestHttp,
    OperationKind, PostgresExtension, PostgresObjectAction, PostgresObjectPage,
    PostgresObjectPageQuery, PostgresObjectPreview, PostgresOwnedObject, PostgresOwnedObjectKind,
    PostgresPartition, PostgresPolicy, PostgresRole, PostgresSchemaGrant, PostgresSchemaPrivilege,
    SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const EXTENSIONS_SQL: &str = "SELECT a.name::text, a.installed_version::text, a.default_version::text, n.nspname::text FROM pg_catalog.pg_available_extensions a LEFT JOIN pg_catalog.pg_extension e ON e.extname = a.name LEFT JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace ORDER BY a.name LIMIT $1::bigint OFFSET $2::bigint";
const PARTITIONS_SQL: &str = "SELECT pn.nspname::text, p.relname::text, cn.nspname::text, c.relname::text, pg_catalog.pg_get_expr(c.relpartbound, c.oid)::text FROM pg_catalog.pg_inherits i JOIN pg_catalog.pg_class p ON p.oid = i.inhparent JOIN pg_catalog.pg_namespace pn ON pn.oid = p.relnamespace JOIN pg_catalog.pg_class c ON c.oid = i.inhrelid JOIN pg_catalog.pg_namespace cn ON cn.oid = c.relnamespace WHERE p.relkind = 'p' AND pg_catalog.has_table_privilege(p.oid, 'SELECT') AND pg_catalog.has_table_privilege(c.oid, 'SELECT') ORDER BY pn.nspname, p.relname, cn.nspname, c.relname LIMIT $1::bigint OFFSET $2::bigint";
const ROLES_SQL: &str = "SELECT rolname::text, rolcanlogin::text, rolcreaterole::text, rolsuper::text FROM pg_catalog.pg_roles ORDER BY rolname LIMIT $1::bigint OFFSET $2::bigint";
const OWNERS_SQL: &str = "SELECT kind, name, owner FROM (SELECT 'database'::text AS kind, datname::text AS name, pg_catalog.pg_get_userbyid(datdba)::text AS owner FROM pg_catalog.pg_database WHERE datname = current_database() UNION ALL SELECT 'schema'::text, nspname::text, pg_catalog.pg_get_userbyid(nspowner)::text FROM pg_catalog.pg_namespace WHERE nspname <> 'information_schema' AND nspname NOT LIKE 'pg\\_%' ESCAPE '\\') owned ORDER BY kind, name LIMIT $1::bigint OFFSET $2::bigint";
const SCHEMA_GRANTS_SQL: &str = "SELECT n.nspname::text, CASE WHEN acl.grantee = 0 THEN 'PUBLIC' ELSE pg_catalog.pg_get_userbyid(acl.grantee) END::text, acl.privilege_type::text, acl.is_grantable::text FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL pg_catalog.aclexplode(n.nspacl) acl WHERE n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%' ESCAPE '\\' AND (pg_catalog.has_schema_privilege(n.oid, 'USAGE') OR n.nspowner = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = current_user)) ORDER BY n.nspname, grantee, acl.privilege_type LIMIT $1::bigint OFFSET $2::bigint";
const POLICIES_SQL: &str = "SELECT n.nspname::text, c.relname::text, p.polname::text, p.polcmd::text, p.polpermissive::text, left((SELECT string_agg(CASE WHEN role_oid = 0 THEN 'PUBLIC' ELSE pg_catalog.pg_get_userbyid(role_oid) END, ', ' ORDER BY role_oid) FROM unnest(p.polroles) role_oid), 1024)::text, (coalesce(length((SELECT string_agg(CASE WHEN role_oid = 0 THEN 'PUBLIC' ELSE pg_catalog.pg_get_userbyid(role_oid) END, ', ' ORDER BY role_oid) FROM unnest(p.polroles) role_oid)),0)>1024)::text, left(pg_catalog.pg_get_expr(p.polqual,p.polrelid),2048)::text, (coalesce(length(pg_catalog.pg_get_expr(p.polqual,p.polrelid)),0)>2048)::text, left(pg_catalog.pg_get_expr(p.polwithcheck,p.polrelid),2048)::text, (coalesce(length(pg_catalog.pg_get_expr(p.polwithcheck,p.polrelid)),0)>2048)::text, c.relrowsecurity::text, c.relforcerowsecurity::text FROM pg_catalog.pg_policy p JOIN pg_catalog.pg_class c ON c.oid=p.polrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind IN ('r','p') AND (pg_catalog.has_table_privilege(c.oid,'SELECT') OR pg_catalog.pg_has_role(current_user,c.relowner,'USAGE')) ORDER BY n.nspname,c.relname,p.polname LIMIT $1::bigint OFFSET $2::bigint";

pub async fn roles(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresRole>> {
    let (rows, limit) = read(store, session, connection, query.clone(), ROLES_SQL).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 4 {
            return Err(invalid_shape());
        }
        Ok(PostgresRole {
            name: required(&values[0])?,
            can_login: boolean(&values[1])?,
            can_create_role: boolean(&values[2])?,
            superuser: boolean(&values[3])?,
        })
    })
}

pub async fn owners(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresOwnedObject>> {
    let (rows, limit) = read(store, session, connection, query.clone(), OWNERS_SQL).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 3 {
            return Err(invalid_shape());
        }
        let kind = match required(&values[0])?.as_str() {
            "database" => PostgresOwnedObjectKind::Database,
            "schema" => PostgresOwnedObjectKind::Schema,
            _ => return Err(invalid_shape()),
        };
        Ok(PostgresOwnedObject {
            kind,
            name: required(&values[1])?,
            owner: required(&values[2])?,
        })
    })
}

pub async fn schema_grants(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresSchemaGrant>> {
    let (rows, limit) = read(store, session, connection, query.clone(), SCHEMA_GRANTS_SQL).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 4 {
            return Err(invalid_shape());
        }
        Ok(PostgresSchemaGrant {
            schema: required(&values[0])?,
            grantee: required(&values[1])?,
            privilege: required(&values[2])?,
            grantable: boolean(&values[3])?,
        })
    })
}

fn boolean(value: &Value) -> ApiResult<bool> {
    match value {
        Value::Text(value) if value == "true" => Ok(true),
        Value::Text(value) if value == "false" => Ok(false),
        _ => Err(invalid_shape()),
    }
}

pub async fn extensions(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresExtension>> {
    let (rows, limit) = read(store, session, connection, query.clone(), EXTENSIONS_SQL).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 4 {
            return Err(invalid_shape());
        }
        Ok(PostgresExtension {
            name: required(&values[0])?,
            installed_version: optional(&values[1])?,
            default_version: optional(&values[2])?,
            schema: optional(&values[3])?,
        })
    })
}

pub async fn partitions(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresPartition>> {
    let (rows, limit) = read(store, session, connection, query.clone(), PARTITIONS_SQL).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 5 {
            return Err(invalid_shape());
        }
        Ok(PostgresPartition {
            parent_schema: required(&values[0])?,
            parent: required(&values[1])?,
            child_schema: required(&values[2])?,
            child: required(&values[3])?,
            bound: optional(&values[4])?,
        })
    })
}

pub async fn policies(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresPolicy>> {
    let (rows, limit) = read(store, session, connection, query.clone(), POLICIES_SQL).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 13 {
            return Err(invalid_shape());
        }
        Ok(PostgresPolicy {
            schema: required(&values[0])?,
            table: required(&values[1])?,
            name: required(&values[2])?,
            command: required(&values[3])?,
            permissive: boolean(&values[4])?,
            roles: required(&values[5])?,
            roles_truncated: boolean(&values[6])?,
            using_expression: optional(&values[7])?,
            using_truncated: boolean(&values[8])?,
            check_expression: optional(&values[9])?,
            check_truncated: boolean(&values[10])?,
            row_security_enabled: boolean(&values[11])?,
            row_security_forced: boolean(&values[12])?,
        })
    })
}

async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
    sql: &str,
) -> ApiResult<(Vec<sift_protocol::Row>, u32)> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "PostgreSQL workbench requires a PostgreSQL connection",
        )
        .into());
    }
    let limit = query.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) || query.offset > 10_000 {
        return Err(ApiError::BadRequest(
            "object page must use limit 1..=200 and offset <=10000".into(),
        ));
    }
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params: vec![
                    Value::Int64(i64::from(limit + 1)),
                    Value::Int64(i64::from(query.offset)),
                ],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ListPostgresObjects,
        )
        .await?;
    Ok((response.rows, limit))
}

fn page<T>(
    rows: Vec<sift_protocol::Row>,
    offset: u32,
    limit: u32,
    parse: impl Fn(&[Value]) -> ApiResult<T>,
) -> ApiResult<PostgresObjectPage<T>> {
    let next_offset =
        (rows.len() > limit as usize && offset + limit <= 10_000).then_some(offset + limit);
    let items = rows
        .iter()
        .take(limit as usize)
        .map(|row| parse(&row.values))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(PostgresObjectPage { items, next_offset })
}

fn required(value: &Value) -> ApiResult<String> {
    match value {
        Value::Text(value) => Ok(value.clone()),
        _ => Err(invalid_shape()),
    }
}

fn optional(value: &Value) -> ApiResult<Option<String>> {
    match value {
        Value::Text(value) => Ok(Some(value.clone())),
        Value::Null => Ok(None),
        _ => Err(invalid_shape()),
    }
}

fn invalid_shape() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "PostgreSQL workbench returned an unexpected catalog shape",
    )
    .into()
}

const EXTENSION_STATE_SQL: &str = "SELECT a.name::text, a.installed_version::text, a.default_version::text FROM pg_catalog.pg_available_extensions a WHERE a.name = $1::text";
const PARTITION_STATE_SQL: &str = "SELECT p.oid::text, c.oid::text, pg_catalog.pg_get_expr(c.relpartbound, c.oid)::text FROM pg_catalog.pg_inherits i JOIN pg_catalog.pg_class p ON p.oid = i.inhparent JOIN pg_catalog.pg_namespace pn ON pn.oid = p.relnamespace JOIN pg_catalog.pg_class c ON c.oid = i.inhrelid JOIN pg_catalog.pg_namespace cn ON cn.oid = c.relnamespace WHERE p.relkind = 'p' AND pg_catalog.has_table_privilege(p.oid, 'SELECT') AND pg_catalog.has_table_privilege(c.oid, 'SELECT') AND pn.nspname = $1::text AND p.relname = $2::text AND cn.nspname = $3::text AND c.relname = $4::text";
const POLICY_STATE_SQL: &str = "SELECT p.oid::text,c.oid::text,p.polname::text,md5(coalesce(p.polqual::text,'')),md5(coalesce(p.polwithcheck::text,'')),p.polroles::text,p.polcmd::text,p.polpermissive::text,c.relrowsecurity::text,c.relforcerowsecurity::text,c.relowner::text,(pg_catalog.pg_has_role(current_user,c.relowner,'USAGE') OR current_setting('is_superuser')='on')::text,EXISTS(SELECT 1 FROM pg_catalog.pg_policy target WHERE target.polrelid=c.oid AND target.polname=$4::text)::text FROM pg_catalog.pg_policy p JOIN pg_catalog.pg_class c ON c.oid=p.polrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1::text AND c.relname=$2::text AND p.polname=$3::text AND c.relkind IN ('r','p')";

pub async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "PostgreSQL workbench requires a PostgreSQL connection",
        )
        .into());
    }
    if is_admin_action(&action) {
        if matches!(action, PostgresObjectAction::RenamePolicy { .. }) {
            return policy_preview(store, session, connection, action).await;
        }
        return admin_preview(store, session, connection, action).await;
    }
    let (lookup, params, sql, warning) = match &action {
        PostgresObjectAction::InstallExtension { name } => {
            let name = identifier(name)?;
            (EXTENSION_STATE_SQL, vec![Value::Text(name.into())], format!("CREATE EXTENSION {}", quote(name)),
             "Installing an extension can run extension-owned scripts and create database objects".to_string())
        }
        PostgresObjectAction::DropExtension { name } => {
            let name = identifier(name)?;
            (EXTENSION_STATE_SQL, vec![Value::Text(name.into())], format!("DROP EXTENSION {} RESTRICT", quote(name)),
             "Dropping an extension removes its member objects; RESTRICT rejects external dependencies".to_string())
        }
        PostgresObjectAction::DetachPartition {
            parent_schema,
            parent,
            child_schema,
            child,
        } => {
            let parent_schema = identifier(parent_schema)?;
            let parent = identifier(parent)?;
            let child_schema = identifier(child_schema)?;
            let child = identifier(child)?;
            (
                PARTITION_STATE_SQL,
                vec![
                    Value::Text(parent_schema.into()),
                    Value::Text(parent.into()),
                    Value::Text(child_schema.into()),
                    Value::Text(child.into()),
                ],
                format!(
                    "ALTER TABLE {}.{} DETACH PARTITION {}.{}",
                    quote(parent_schema),
                    quote(parent),
                    quote(child_schema),
                    quote(child)
                ),
                "Detaching a partition changes routing and leaves the child as an ordinary table"
                    .to_string(),
            )
        }
        _ => unreachable!("administrative action handled above"),
    };
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: lookup.into(),
                params,
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::PreviewPostgresObject,
        )
        .await?;
    let state = response.rows.first().map(|row| &row.values);
    match (&action, state) {
        (PostgresObjectAction::InstallExtension { .. }, Some(values))
            if values.len() == 3 && matches!(values[1], Value::Null) => {}
        (PostgresObjectAction::DropExtension { .. }, Some(values))
            if values.len() == 3 && !matches!(values[1], Value::Null) => {}
        (PostgresObjectAction::DetachPartition { .. }, Some(values)) if values.len() == 3 => {}
        _ => {
            return Err(ApiError::BadRequest(
                "PostgreSQL object is unavailable or already in the requested state".into(),
            ))
        }
    }
    let state =
        serde_json::to_vec(state.ok_or_else(invalid_shape)?).map_err(|_| invalid_shape())?;
    let mut digest = Sha256::new();
    digest.update(sql.as_bytes());
    digest.update(&state);
    let precondition = format!("{:x}", digest.finalize());
    Ok(PostgresObjectPreview {
        action,
        sql,
        precondition,
        warning,
    })
}

pub async fn apply(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: ApplyPostgresObjectRequest,
) -> ApiResult<()> {
    if !request.confirmed || request.precondition.len() != 64 {
        return Err(ApiError::BadRequest(
            "A current preview and explicit confirmation are required".into(),
        ));
    }
    if is_admin_action(&request.action) && !request.production_confirmed {
        return Err(ApiError::BadRequest(
            "Administrative changes require explicit production confirmation".into(),
        ));
    }
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ApplyPostgresObject,
        None,
        &[],
    )?;
    let fresh = preview(store, session, connection, request.action).await?;
    if request.precondition != fresh.precondition {
        return Err(ApiError::BadRequest(
            "PostgreSQL object changed since preview; refresh before applying".into(),
        ));
    }
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteQuery,
        Some(&fresh.sql),
        &[],
    )?;
    store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: fresh.sql,
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ApplyPostgresObject,
        )
        .await?;
    Ok(())
}

fn is_admin_action(action: &PostgresObjectAction) -> bool {
    matches!(
        action,
        PostgresObjectAction::CreateRole { .. }
            | PostgresObjectAction::GrantSchemaPrivilege { .. }
            | PostgresObjectAction::RevokeSchemaPrivilege { .. }
            | PostgresObjectAction::ChangeOwner { .. }
            | PostgresObjectAction::RenamePolicy { .. }
    )
}

async fn policy_preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    let PostgresObjectAction::RenamePolicy {
        schema,
        table,
        name,
        new_name,
    } = &action
    else {
        unreachable!("policy preview receives only rename actions")
    };
    let (schema, table, name, new_name) = (
        identifier(schema)?,
        identifier(table)?,
        identifier(name)?,
        identifier(new_name)?,
    );
    if name == new_name {
        return Err(ApiError::BadRequest("Policy already has that name".into()));
    }
    let sql = format!(
        "ALTER POLICY {} ON {}.{} RENAME TO {}",
        quote(name),
        quote(schema),
        quote(table),
        quote(new_name)
    );
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: POLICY_STATE_SQL.into(),
                params: vec![
                    Value::Text(schema.into()),
                    Value::Text(table.into()),
                    Value::Text(name.into()),
                    Value::Text(new_name.into()),
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
    let state = response
        .rows
        .first()
        .map(|row| &row.values)
        .ok_or_else(|| ApiError::BadRequest("PostgreSQL policy was not found".into()))?;
    if state.len() != 13 {
        return Err(invalid_shape());
    }
    if !boolean(&state[11])? {
        return Err(ApiError::Forbidden(
            "PostgreSQL table ownership is required".into(),
        ));
    }
    if boolean(&state[12])? {
        return Err(ApiError::BadRequest(
            "A policy with the target name already exists".into(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(sql.as_bytes());
    digest.update(serde_json::to_vec(state).map_err(|_| invalid_shape())?);
    Ok(PostgresObjectPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning:
            "Renaming a policy changes its name but preserves its command, roles, and expressions"
                .into(),
    })
}

async fn admin_preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    const ROLE_STATE: &str = "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $1::text)::text, COALESCE((SELECT (rolcreaterole OR rolsuper)::text FROM pg_catalog.pg_roles WHERE rolname = current_user), 'false')";
    const SCHEMA_STATE: &str = "SELECT n.oid::text, COALESCE(n.nspacl::text, ''), (n.nspowner = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = current_user) OR current_setting('is_superuser') = 'on')::text, EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $2::text)::text FROM pg_catalog.pg_namespace n WHERE n.nspname = $1::text";
    const OWNER_SCHEMA_STATE: &str = "SELECT n.oid::text, n.nspowner::text, (n.nspowner = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = current_user) OR current_setting('is_superuser') = 'on')::text, EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $2::text)::text FROM pg_catalog.pg_namespace n WHERE n.nspname = $1::text";
    const OWNER_DATABASE_STATE: &str = "SELECT d.oid::text, d.datdba::text, (d.datdba = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = current_user) OR current_setting('is_superuser') = 'on')::text, EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $2::text)::text FROM pg_catalog.pg_database d WHERE d.datname = $1::text AND d.datname = current_database()";
    let (lookup, params, sql, warning, authority_index, existence_index) = match &action {
        PostgresObjectAction::CreateRole { name } => {
            let name = identifier(name)?;
            (
                ROLE_STATE,
                vec![Value::Text(name.into())],
                format!("CREATE ROLE {} NOLOGIN", quote(name)),
                "Creating a role changes database-wide authorization".to_string(),
                1,
                0,
            )
        }
        PostgresObjectAction::GrantSchemaPrivilege {
            schema,
            grantee,
            privilege,
        }
        | PostgresObjectAction::RevokeSchemaPrivilege {
            schema,
            grantee,
            privilege,
        } => {
            let schema = identifier(schema)?;
            let grantee = identifier(grantee)?;
            let verb = if matches!(action, PostgresObjectAction::GrantSchemaPrivilege { .. }) {
                "GRANT"
            } else {
                "REVOKE"
            };
            let right = match privilege {
                PostgresSchemaPrivilege::Usage => "USAGE",
                PostgresSchemaPrivilege::Create => "CREATE",
            };
            let sql = if verb == "GRANT" {
                format!(
                    "GRANT {right} ON SCHEMA {} TO {}",
                    quote(schema),
                    quote(grantee)
                )
            } else {
                format!(
                    "REVOKE {right} ON SCHEMA {} FROM {}",
                    quote(schema),
                    quote(grantee)
                )
            };
            (
                SCHEMA_STATE,
                vec![Value::Text(schema.into()), Value::Text(grantee.into())],
                sql,
                "Changing a schema grant changes access for the named role and its members"
                    .to_string(),
                2,
                3,
            )
        }
        PostgresObjectAction::ChangeOwner {
            object_kind,
            name,
            new_owner,
        } => {
            let name = identifier(name)?;
            let new_owner = identifier(new_owner)?;
            let (catalog, object) = match object_kind {
                PostgresOwnedObjectKind::Database => (OWNER_DATABASE_STATE, "DATABASE"),
                PostgresOwnedObjectKind::Schema => (OWNER_SCHEMA_STATE, "SCHEMA"),
            };
            (
                catalog,
                vec![Value::Text(name.into()), Value::Text(new_owner.into())],
                format!(
                    "ALTER {object} {} OWNER TO {}",
                    quote(name),
                    quote(new_owner)
                ),
                "Changing ownership transfers administrative control of the object".to_string(),
                2,
                3,
            )
        }
        _ => unreachable!("only administrative actions reach this function"),
    };
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: lookup.into(),
                params,
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::PreviewPostgresObject,
        )
        .await?;
    let state = response
        .rows
        .first()
        .map(|row| &row.values)
        .ok_or_else(|| {
            ApiError::BadRequest("PostgreSQL administration object was not found".into())
        })?;
    if state.len() <= authority_index || state.len() <= existence_index {
        return Err(invalid_shape());
    }
    if !boolean(&state[authority_index])? {
        return Err(ApiError::Forbidden(
            "PostgreSQL role or object ownership is required".into(),
        ));
    }
    let exists = boolean(&state[existence_index])?;
    if matches!(action, PostgresObjectAction::CreateRole { .. }) == exists {
        return Err(ApiError::BadRequest(
            "PostgreSQL role is absent or already exists".into(),
        ));
    }
    let state = serde_json::to_vec(state).map_err(|_| invalid_shape())?;
    let mut digest = Sha256::new();
    digest.update(sql.as_bytes());
    digest.update(&state);
    Ok(PostgresObjectPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning,
    })
}

fn identifier(input: &str) -> ApiResult<&str> {
    if input.is_empty() || input.len() > 63 || input.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "PostgreSQL identifier must be 1..=63 bytes without control characters".into(),
        ));
    }
    Ok(input)
}

fn quote(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_quoted_as_single_identifiers() {
        assert_eq!(
            quote("a\"; DROP TABLE users; --"),
            "\"a\"\"; DROP TABLE users; --\""
        );
        assert!(identifier("").is_err());
        assert!(identifier("x\n").is_err());
        assert!(identifier(&"x".repeat(64)).is_err());
    }

    #[test]
    fn object_pages_are_bounded_and_explicitly_paginated() {
        let rows = (0..3)
            .map(|index| sift_protocol::Row::new(vec![Value::Text(index.to_string())]))
            .collect();
        let page = page(rows, 10, 2, |values| required(&values[0])).unwrap();
        assert_eq!(page.items, ["0", "1"]);
        assert_eq!(page.next_offset, Some(12));
    }
}
