//! Native object-ACL review and statement-atomic grant transitions.
use super::*;
use serde::{Deserialize, Serialize};
use sift_protocol::{
    PostgresObjectPrivilege as Privilege, PostgresPrivilegeChange as Change,
    PostgresPrivilegeGrantee as Grantee, PostgresPrivilegeObjectKind as Kind,
    PostgresPrivilegeTarget as Target,
};
const STATE: &str = include_str!("state.sql");
const MAX_STATE: usize = 128 * 1024;
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Edge {
    grantor: u32,
    grantee: u32,
    privilege: String,
    grantable: bool,
}
#[derive(Deserialize)]
struct Grantor {
    oid: u32,
    name: String,
}
#[derive(Deserialize)]
struct Column {
    identity: serde_json::Value,
    grants: Vec<Edge>,
}
#[derive(Deserialize)]
struct State {
    database: String,
    actor: String,
    identity: serde_json::Value,
    version: u32,
    can_grant: bool,
    target_sql: String,
    column_name: Option<String>,
    grantee_oid: u32,
    grantee_sql: String,
    grantors: Vec<Grantor>,
    grants: Vec<Edge>,
    parent_grants: Vec<Edge>,
    columns: Vec<Column>,
}
struct Prepared {
    preview: PostgresObjectPreview,
    guard: String,
}
pub(super) fn handles(action: &PostgresObjectAction) -> bool {
    matches!(action, PostgresObjectAction::ChangeObjectPrivilege { .. })
}
fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Database => "database",
        Kind::Schema => "schema",
        Kind::Table => "table",
        Kind::Column => "column",
        Kind::Sequence => "sequence",
        Kind::Routine => "routine",
    }
}
fn literal(value: &str) -> String {
    format!("E'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}
fn inputs(
    target: &Target,
    grantee: &Grantee,
    privilege: Privilege,
    change: Change,
) -> ApiResult<Vec<Value>> {
    if target.object_oid == 0
        || !privilege.allowed_for(target.kind)
        || (target.kind == Kind::Column && !target.column_number.is_some_and(|n| n > 0))
        || (target.kind != Kind::Column && target.column_number.is_some())
    {
        return Err(bad(
            "Use a native object identity and a privilege supported by its kind",
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
        Value::Text(kind_name(target.kind).into()),
        Value::Int64(i64::from(target.object_oid)),
        target
            .column_number
            .map(|n| Value::Int64(i64::from(n)))
            .unwrap_or(Value::Null),
        recipient,
        Value::Text(privilege.sql_name().into()),
    ])
}
fn transition(edges: &mut Vec<Edge>, grantor: u32, grantee: u32, privilege: &str, change: Change) {
    let matches = |edge: &Edge| {
        edge.grantor == grantor && edge.grantee == grantee && edge.privilege == privilege
    };
    match change {
        Change::Grant { grant_option } => {
            if let Some(edge) = edges.iter_mut().find(|edge| matches(edge)) {
                edge.grantable |= grant_option;
            } else {
                edges.push(Edge {
                    grantor,
                    grantee,
                    privilege: privilege.into(),
                    grantable: grant_option,
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
}
fn compile(
    state: &State,
    target: &Target,
    privilege: Privilege,
    change: Change,
) -> ApiResult<(String, serde_json::Value)> {
    if privilege == Privilege::Maintain && state.version < 170000 {
        return Err(bad("MAINTAIN requires PostgreSQL 17 or newer"));
    }
    if !state.can_grant {
        return Err(bad(
            "The connected native role lacks the required grant option",
        ));
    }
    let [grantor] = state.grantors.as_slice() else {
        return Err(bad(
            "Native grantor is unavailable or ambiguous; SET ROLE explicitly and reopen review",
        ));
    };
    let right = privilege.sql_name();
    let edge = state.grants.iter().find(|edge| {
        edge.grantor == grantor.oid && edge.grantee == state.grantee_oid && edge.privilege == right
    });
    let no_op = match change {
        Change::Grant { grant_option } => edge.is_some_and(|edge| !grant_option || edge.grantable),
        Change::Revoke { grant_option_only } => !state
            .grants
            .iter()
            .chain(
                state
                    .columns
                    .iter()
                    .filter(|_| target.kind == Kind::Table)
                    .flat_map(|column| &column.grants),
            )
            .any(|edge| {
                edge.grantor == grantor.oid
                    && edge.grantee == state.grantee_oid
                    && edge.privilege == right
                    && (!grant_option_only || edge.grantable)
            }),
    };
    if no_op {
        return Err(bad("The selected native grantor edge is already in the requested state; other grantors or inherited access may remain"));
    }
    let right = state.column_name.as_ref().map_or(right.into(), |column| {
        format!("{right} ({})", quote(column))
    });
    let sql = match change {
        Change::Grant { grant_option } => format!(
            "GRANT {right} ON {} TO {}{}",
            state.target_sql,
            state.grantee_sql,
            if grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            }
        ),
        Change::Revoke { grant_option_only } => format!(
            "REVOKE {}{right} ON {} FROM {} RESTRICT",
            if grant_option_only {
                "GRANT OPTION FOR "
            } else {
                ""
            },
            state.target_sql,
            state.grantee_sql
        ),
    };
    let mut grants = state.grants.clone();
    transition(
        &mut grants,
        grantor.oid,
        state.grantee_oid,
        privilege.sql_name(),
        change,
    );
    let columns = state
        .columns
        .iter()
        .map(|column| {
            let mut grants = column.grants.clone();
            if target.kind == Kind::Table && matches!(change, Change::Revoke { .. }) {
                transition(
                    &mut grants,
                    grantor.oid,
                    state.grantee_oid,
                    privilege.sql_name(),
                    change,
                );
            }
            serde_json::json!({"identity":column.identity,"grants":grants})
        })
        .collect::<Vec<_>>();
    Ok((
        sql,
        serde_json::json!({"identity":state.identity,"grants":grants,"parent_grants":state.parent_grants,"columns":columns}),
    ))
}
fn guard_query(params: &[Value]) -> ApiResult<String> {
    let mut remaining = STATE.trim().trim_end_matches(';');
    let mut query = String::new();
    while let Some(index) = remaining.find('$') {
        query.push_str(&remaining[..index]);
        let number = remaining
            .as_bytes()
            .get(index + 1)
            .copied()
            .ok_or_else(invalid_shape)?;
        let value = number
            .checked_sub(b'1')
            .and_then(|n| params.get(usize::from(n)))
            .ok_or_else(invalid_shape)?;
        match value {
            Value::Null => query.push_str("NULL"),
            Value::Text(value) => query.push_str(&literal(value)),
            Value::Int64(value) => query.push_str(&value.to_string()),
            _ => return Err(invalid_shape()),
        }
        remaining = &remaining[index + 2..];
    }
    query.push_str(remaining);
    Ok(format!("({query})::jsonb"))
}

async fn prepare(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<Prepared> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Object privileges require PostgreSQL",
        )
        .into());
    }
    let PostgresObjectAction::ChangeObjectPrivilege {
        target,
        grantee,
        privilege,
        change,
    } = &action
    else {
        return Err(invalid_shape());
    };
    let params = inputs(target, grantee, *privilege, *change)?;
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
    let raw = response
        .rows
        .first()
        .and_then(|row| row.values.first())
        .and_then(|value| {
            if let Value::Text(value) = value {
                Some(value)
            } else {
                None
            }
        })
        .ok_or_else(|| bad("Native object, grantee or bounded ACL state is unavailable"))?;
    if raw.len() > MAX_STATE {
        return Err(bad("Native privilege review state exceeds 128 KiB"));
    }
    let state: State = serde_json::from_str(raw).map_err(|_| invalid_shape())?;
    let (sql, expected) = compile(&state, target, *privilege, *change)?;
    let mut digest = Sha256::new();
    digest.update(b"native-object-privilege-v1");
    digest.update(session.0.to_be_bytes());
    digest.update(connection.0.to_be_bytes());
    digest.update(sql.as_bytes());
    digest.update(raw.as_bytes());
    let lookup = guard_query(&params)?;
    let expected = serde_json::to_string(&expected).map_err(|_| invalid_shape())?;
    let body=format!("DECLARE observed jsonb; expected jsonb := {}::jsonb; BEGIN SELECT {lookup} INTO observed; IF observed IS DISTINCT FROM {}::jsonb THEN RAISE EXCEPTION 'Privilege review changed before native execution'; END IF; EXECUTE {}; SELECT {lookup} INTO observed; IF observed IS NULL OR observed->'identity' IS DISTINCT FROM expected->'identity' OR observed->'grants' IS DISTINCT FROM expected->'grants' OR observed->'parent_grants' IS DISTINCT FROM expected->'parent_grants' OR (SELECT COALESCE(jsonb_agg(jsonb_build_object('identity',c->'identity','grants',c->'grants') ORDER BY ord),'[]'::jsonb) FROM jsonb_array_elements(observed->'columns') WITH ORDINALITY x(c,ord)) IS DISTINCT FROM expected->'columns' THEN RAISE EXCEPTION 'Native privilege outcome differed from reviewed transition'; END IF; END",literal(&expected),literal(raw),literal(&sql));
    let grantor = &state.grantors[0];
    Ok(Prepared {preview:PostgresObjectPreview {action,sql,precondition:format!("{:x}",digest.finalize()),warning:format!("Native database {:?}, actor {:?}; grantor {:?} (OID {}), recipient {} (OID {}). RESTRICT refuses dependent revocations. Other grantors, PUBLIC, membership and owner rights can preserve access. Table revocation also affects corresponding column grants. Existing database connections are not evicted by CONNECT revocation. Review exact native identities and SQL.",state.database,state.actor,grantor.name,grantor.oid,state.grantee_sql,state.grantee_oid),impact:None},guard:format!("DO {} LANGUAGE plpgsql",literal(&body))})
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
    let prepared = prepare(store, session, connection, request.action).await?;
    if prepared.preview.precondition != request.precondition {
        return Err(bad(
            "PostgreSQL privileges changed since preview; refresh before applying",
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
    fn state() -> State {
        serde_json::from_value(serde_json::json!({
            "database":"db","actor":"actor","identity":[1,"db",3,"actor"],"version":170010,"can_grant":true,
            "target_sql":"TABLE \"schema\".\"table\"","column_name":null,"grantee_oid":8,"grantee_sql":"\"recipient\"",
            "grantors":[{"oid":7,"name":"owner"}],"grants":[],"parent_grants":[],
            "columns":[{"identity":[1,"col",23,-1],"grants":[
                {"grantor":7,"grantee":8,"privilege":"SELECT","grantable":true},
                {"grantor":9,"grantee":8,"privilege":"SELECT","grantable":false}
            ]}]
        })).unwrap()
    }
    #[test]
    fn native_transitions_keep_other_grantors_and_refuse_ambiguous_authority() {
        let target = Target {
            kind: Kind::Table,
            object_oid: 10,
            column_number: None,
        };
        let mut state = state();
        let (sql, after) = compile(
            &state,
            &target,
            Privilege::Select,
            Change::Grant { grant_option: true },
        )
        .unwrap();
        assert_eq!(
            sql,
            "GRANT SELECT ON TABLE \"schema\".\"table\" TO \"recipient\" WITH GRANT OPTION"
        );
        assert_eq!(after["grants"][0]["grantable"], true);
        assert_eq!(after["columns"][0]["grants"].as_array().unwrap().len(), 2);
        state.grants = serde_json::from_value(after["grants"].clone()).unwrap();
        assert!(compile(
            &state,
            &target,
            Privilege::Select,
            Change::Grant {
                grant_option: false
            }
        )
        .is_err());
        let (sql, after) = compile(
            &state,
            &target,
            Privilege::Select,
            Change::Revoke {
                grant_option_only: false,
            },
        )
        .unwrap();
        assert!(sql.ends_with(" RESTRICT"));
        assert!(after["grants"].as_array().unwrap().is_empty());
        assert_eq!(after["columns"][0]["grants"].as_array().unwrap().len(), 1);
        assert_eq!(after["columns"][0]["grants"][0]["grantor"], 9);
        let (_, after) = compile(
            &state,
            &target,
            Privilege::Select,
            Change::Revoke {
                grant_option_only: true,
            },
        )
        .unwrap();
        assert_eq!(after["grants"][0]["grantable"], false);
        assert_eq!(after["columns"][0]["grants"][0]["grantable"], false);
        state.grantors.push(Grantor {
            oid: 9,
            name: "other".into(),
        });
        assert!(compile(
            &state,
            &target,
            Privilege::Update,
            Change::Grant {
                grant_option: false
            }
        )
        .is_err());
        assert!(inputs(
            &target,
            &Grantee::Public {},
            Privilege::Select,
            Change::Grant { grant_option: true }
        )
        .is_err());
        assert!(inputs(
            &Target {
                kind: Kind::Routine,
                ..target.clone()
            },
            &Grantee::Public {},
            Privilege::Select,
            Change::Grant {
                grant_option: false
            }
        )
        .is_err());
        assert!(inputs(
            &Target {
                kind: Kind::Column,
                ..target
            },
            &Grantee::Public {},
            Privilege::Select,
            Change::Grant {
                grant_option: false
            }
        )
        .is_err());
    }
    #[test]
    fn native_guard_literals_do_not_reinterpret_dollar_parameters_or_backslashes() {
        assert!(serde_json::from_value::<Grantee>(
            serde_json::json!({"kind":"public","name":"not-a-role"})
        )
        .is_err());

        let role = "role $1 $5 ' \\ $native$";
        let params = inputs(
            &Target {
                kind: Kind::Table,
                object_oid: 10,
                column_number: None,
            },
            &Grantee::Role { name: role.into() },
            Privilege::Select,
            Change::Grant {
                grant_option: false,
            },
        )
        .unwrap();
        let query = guard_query(&params).unwrap();
        assert!(query.contains(&literal(role)));
        assert!(query.contains("10::bigint::oid"));
        assert!(literal(role).starts_with("E'"));
        assert!(literal(role).contains("''"));
    }
}
