//! Statement-atomic native default-ACL transitions; never retroactive grants.
use super::*;
use serde::{Deserialize, Serialize};
use sift_protocol::{
    PostgresDefaultPrivilegeKind as Kind, PostgresObjectPrivilege as Privilege,
    PostgresPrivilegeChange as Change, PostgresPrivilegeGrantee as Grantee,
};
const STATE: &str = include_str!("default-privilege-state.sql");
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Edge {
    grantor: u32,
    grantee: u32,
    privilege: String,
    grantable: bool,
    grantor_name: String,
    grantee_name: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct State {
    version: u32,
    identity: serde_json::Value,
    creator_oid: u32,
    recipient_oid: u32,
    can_change: bool,
    global_grants: Vec<Edge>,
    schema_grants: Vec<Edge>,
}
struct Prepared {
    preview: PostgresObjectPreview,
    guard: String,
}
pub(super) fn handles(action: &PostgresObjectAction) -> bool {
    matches!(action, PostgresObjectAction::ChangeDefaultPrivilege { .. })
}
fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn literal(value: &str) -> String {
    format!("E'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}
fn inputs(action: &PostgresObjectAction) -> ApiResult<Vec<Value>> {
    let PostgresObjectAction::ChangeDefaultPrivilege {
        creator,
        schema,
        object_kind,
        grantee,
        privilege,
        change,
    } = action
    else {
        return Err(bad("Expected a default privilege action"));
    };
    identifier(creator)?;
    if creator.starts_with("pg_") {
        return Err(bad("System creator roles are not editable"));
    }
    if let Some(schema) = schema {
        identifier(schema)?;
        if schema == "information_schema" || schema.starts_with("pg_") {
            return Err(bad("System schemas are not editable"));
        }
    }
    if !object_kind.allowed(*privilege) || (*object_kind == Kind::Schemas && schema.is_some()) {
        return Err(bad(
            "Choose a supported privilege and scope for the future object family",
        ));
    }
    let recipient = match grantee {
        Grantee::Role { name } => Value::Text(identifier(name)?.into()),
        Grantee::Public {} => {
            if matches!(change, Change::Grant { grant_option: true }) {
                return Err(bad("PUBLIC cannot receive a grant option"));
            }
            Value::Null
        }
    };
    Ok(vec![
        Value::Text(creator.clone()),
        schema.clone().map(Value::Text).unwrap_or(Value::Null),
        recipient,
        Value::Text(
            match object_kind {
                Kind::Tables => "r",
                Kind::Sequences => "S",
                Kind::Routines => "f",
                Kind::Types => "T",
                Kind::Schemas => "n",
            }
            .into(),
        ),
    ])
}
fn transition(state: &mut State, action: &PostgresObjectAction) -> ApiResult<String> {
    let PostgresObjectAction::ChangeDefaultPrivilege {
        creator,
        schema,
        object_kind,
        grantee,
        privilege,
        change,
    } = action
    else {
        return Err(bad("Expected a default privilege action"));
    };
    if state.version < 160000 || (*privilege == Privilege::Maintain && state.version < 170000) {
        return Err(bad(
            "Default privilege reviews require PostgreSQL 16+; MAINTAIN requires 17+",
        ));
    }
    if !state.can_change {
        return Err(ApiError::Forbidden(
            "Native creator-role authority is required; SET ROLE explicitly if necessary".into(),
        ));
    }
    if state.global_grants.len() > 256 || state.schema_grants.len() > 256 {
        return Err(bad("Default privilege ACL exceeds 256 entries per scope"));
    }
    let edges = if schema.is_some() {
        &mut state.schema_grants
    } else {
        &mut state.global_grants
    };
    let edge = edges.iter().find(|edge| {
        edge.grantor == state.creator_oid
            && edge.grantee == state.recipient_oid
            && edge.privilege == privilege.sql_name()
    });
    let no_op = match change {
        Change::Grant { grant_option } => edge.is_some_and(|edge| !*grant_option || edge.grantable),
        Change::Revoke { grant_option_only } => {
            !edge.is_some_and(|edge| !*grant_option_only || edge.grantable)
        }
    };
    if no_op {
        return Err(bad("Selected default ACL edge is already in that state; per-schema revocation cannot remove global defaults"));
    }
    let recipient = match grantee {
        Grantee::Role { name } => quote(name),
        Grantee::Public {} => "PUBLIC".into(),
    };
    let right = privilege.sql_name();
    let command = match change {
        Change::Grant { grant_option } => format!(
            "GRANT {right} ON {} TO {recipient}{}",
            object_kind.sql_name(),
            if *grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            }
        ),
        Change::Revoke { grant_option_only } => format!(
            "REVOKE {}{right} ON {} FROM {recipient} RESTRICT",
            if *grant_option_only {
                "GRANT OPTION FOR "
            } else {
                ""
            },
            object_kind.sql_name()
        ),
    };
    let matches = |edge: &Edge| {
        edge.grantor == state.creator_oid
            && edge.grantee == state.recipient_oid
            && edge.privilege == right
    };
    match change {
        Change::Grant { grant_option } => {
            if let Some(edge) = edges.iter_mut().find(|edge| matches(edge)) {
                edge.grantable |= *grant_option;
            } else {
                edges.push(Edge {
                    grantor: state.creator_oid,
                    grantee: state.recipient_oid,
                    privilege: right.into(),
                    grantable: *grant_option,
                    grantor_name: creator.clone(),
                    grantee_name: match grantee {
                        Grantee::Role { name } => Some(name.clone()),
                        Grantee::Public {} => None,
                    },
                });
            }
        }
        Change::Revoke {
            grant_option_only: true,
        } => {
            for edge in edges.iter_mut().filter(|edge| matches(edge)) {
                edge.grantable = false;
            }
        }
        Change::Revoke {
            grant_option_only: false,
        } => edges.retain(|edge| !matches(edge)),
    }
    edges.sort_by(|a, b| {
        (a.grantor, a.grantee, &a.privilege).cmp(&(b.grantor, b.grantee, &b.privilege))
    });
    Ok(format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {}{} {command}",
        quote(creator),
        schema
            .as_ref()
            .map_or_else(String::new, |name| format!(" IN SCHEMA {}", quote(name)))
    ))
}
fn lookup(params: &[Value]) -> ApiResult<String> {
    let mut rest = STATE.trim().trim_end_matches(';');
    let mut sql = String::new();
    while let Some(index) = rest.find('$') {
        sql.push_str(&rest[..index]);
        let number = rest.as_bytes().get(index + 1).ok_or_else(invalid_shape)?;
        let value = number
            .checked_sub(b'1')
            .and_then(|n| params.get(n as usize))
            .ok_or_else(invalid_shape)?;
        match value {
            Value::Text(value) => sql.push_str(&literal(value)),
            Value::Null => sql.push_str("NULL"),
            _ => return Err(invalid_shape()),
        }
        rest = &rest[index + 2..];
    }
    sql.push_str(rest);
    Ok(format!("({sql})::pg_catalog.jsonb"))
}
async fn prepare(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<Prepared> {
    let source = store.conn_entry(session, connection)?;
    if source.driver.semantic_engine() != Some(Engine::Postgres) {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Default privileges require PostgreSQL",
        )
        .into());
    }
    let generation = source.execution.review_id;
    let store = store.bind_source(session, connection, source)?;
    let params = inputs(&action)?;
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: STATE.into(),
                params: params.clone(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::PreviewPostgresObject,
        )
        .await?;
    let [Value::Text(raw)] = response
        .rows
        .first()
        .ok_or_else(|| bad("Creator, recipient or schema is unavailable"))?
        .values
        .as_slice()
    else {
        return Err(invalid_shape());
    };
    if raw.len() > 128 * 1024 || response.has_more || response.rows.len() != 1 {
        return Err(bad(
            "Default privilege review exceeds its bounded catalog shape",
        ));
    }
    let mut state: State = serde_json::from_str(raw).map_err(|_| invalid_shape())?;
    let impact = state
        .global_grants
        .iter()
        .map(|e| ("global", e))
        .chain(state.schema_grants.iter().map(|e| ("schema", e)))
        .map(|(scope, e)| {
            format!(
                "{scope}: grantor {:?} (#{}), recipient {} (#{}) · {} · grant option {}",
                e.grantor_name,
                e.grantor,
                e.grantee_name
                    .as_ref()
                    .map_or_else(|| "PUBLIC (all roles)".into(), |name| format!("{name:?}")),
                e.grantee,
                e.privilege,
                e.grantable
            )
        })
        .collect();
    let sql = transition(&mut state, &action)?;
    let expected = serde_json::to_string(&state).map_err(|_| invalid_shape())?;
    let mut digest = Sha256::new();
    digest.update(b"postgres-default-privilege-v1");
    digest.update(generation.as_bytes());
    digest.update(session.0.to_be_bytes());
    digest.update(connection.0.to_be_bytes());
    digest.update(sql.as_bytes());
    digest.update(raw.as_bytes());
    let lookup = lookup(&params)?;
    let body=format!("DECLARE observed pg_catalog.jsonb; BEGIN SELECT {lookup} INTO observed; IF NOT COALESCE(observed OPERATOR(pg_catalog.=) {}::pg_catalog.jsonb,false) THEN RAISE EXCEPTION 'Default privilege review changed'; END IF; EXECUTE {}; SELECT {lookup} INTO observed; IF NOT COALESCE(observed OPERATOR(pg_catalog.=) {}::pg_catalog.jsonb,false) THEN RAISE EXCEPTION 'Default privilege transition differed'; END IF; END",literal(raw),literal(&sql),literal(&expected));
    Ok(Prepared {preview:PostgresObjectPreview {action,sql,precondition:format!("{:x}",digest.finalize()),warning:"Future objects created by this exact role only, in this database. Existing objects are unchanged. Success acknowledges the native statement; caller transactions may still require commit. Per-schema ACLs add to global defaults; revoking a schema edge cannot remove global rights. Role membership does not pass creator defaults to member-created objects. Owner, PUBLIC and inherited access may remain. Review current global/schema ACLs below; native authority and optimistic catalog checks remain authoritative, without locking future creation.".into(),impact:Some(sift_protocol::PostgresObjectImpact {objects:impact,truncated:false})},guard:format!("DO {} LANGUAGE plpgsql",literal(&body))})
}
pub(super) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    Ok(prepare(store, session, connection, action).await?.preview)
}
pub(super) async fn apply(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: ApplyPostgresObjectRequest,
) -> ApiResult<()> {
    let source = store.conn_entry(session, connection)?;
    let store = store.bind_source(session, connection, source)?;
    let prepared = prepare(&store, session, connection, request.action).await?;
    if request.precondition != prepared.preview.precondition {
        return Err(bad(
            "Default privileges changed since preview; refresh before applying",
        ));
    }
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteQuery,
        Some(&prepared.preview.sql),
        &[],
    )?;
    store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: prepared.guard,
                params: vec![],
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

#[cfg(test)]
mod tests {
    use super::*;
    fn action() -> PostgresObjectAction {
        PostgresObjectAction::ChangeDefaultPrivilege {
            creator: "creator".into(),
            schema: Some("schema".into()),
            object_kind: Kind::Tables,
            grantee: Grantee::Role {
                name: "recipient".into(),
            },
            privilege: Privilege::Select,
            change: Change::Grant { grant_option: true },
        }
    }
    fn state() -> State {
        State {
            version: 170010,
            identity: serde_json::json!([1, 2, 3]),
            creator_oid: 2,
            recipient_oid: 3,
            can_change: true,
            global_grants: vec![],
            schema_grants: vec![],
        }
    }
    #[test]
    fn transitions_preserve_other_scope_and_refuse_noops_authority_and_invalid_bounds() {
        let action = action();
        let mut state = state();
        let sql = transition(&mut state, &action).unwrap();
        assert_eq!(sql,"ALTER DEFAULT PRIVILEGES FOR ROLE \"creator\" IN SCHEMA \"schema\" GRANT SELECT ON TABLES TO \"recipient\" WITH GRANT OPTION");
        assert!(state.global_grants.is_empty());
        assert_eq!(state.schema_grants.len(), 1);
        assert!(state.schema_grants[0].grantable);
        assert!(transition(&mut state, &action).is_err());
        state.can_change = false;
        assert!(transition(&mut state, &action).is_err());
        state.can_change = true;
        state.schema_grants = vec![state.schema_grants[0].clone(); 257];
        assert!(transition(&mut state, &action).is_err());
        let mut invalid = action.clone();
        if let PostgresObjectAction::ChangeDefaultPrivilege { grantee, .. } = &mut invalid {
            *grantee = Grantee::Public {};
        }
        assert!(inputs(&invalid).is_err());
        if let PostgresObjectAction::ChangeDefaultPrivilege { object_kind, .. } = &mut invalid {
            *object_kind = Kind::Schemas;
        }
        assert!(inputs(&invalid).is_err());
        let mut quoted = action;
        if let PostgresObjectAction::ChangeDefaultPrivilege { creator, .. } = &mut quoted {
            *creator = "role $1 \\ '".into();
        }
        let lookup = lookup(&inputs(&quoted).unwrap()).unwrap();
        assert!(lookup.contains("E'role $1 \\\\ '''"));
    }
}
