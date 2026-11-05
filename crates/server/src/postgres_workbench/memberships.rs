//! Explicit native membership edges and identity-bound administrative review.
use super::*;
use sift_protocol::{ExecuteResponse, PostgresRoleMembership};

const STATE: &str = include_str!("membership-state.sql");
const LIST: &str = "SELECT r.rolname::text,m.rolname::text,g.rolname::text,a.admin_option::text,a.inherit_option::text,a.set_option::text FROM pg_catalog.pg_auth_members a JOIN pg_catalog.pg_roles r ON r.oid=a.roleid JOIN pg_catalog.pg_roles m ON m.oid=a.member JOIN pg_catalog.pg_roles g ON g.oid=a.grantor ORDER BY r.rolname,m.rolname,g.rolname,a.oid LIMIT $1::bigint OFFSET $2::bigint";

pub(super) fn handles(action: &PostgresObjectAction) -> bool {
    matches!(
        action,
        PostgresObjectAction::GrantRoleMembership { .. }
            | PostgresObjectAction::RevokeRoleMembership { .. }
    )
}
async fn query(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: &str,
    params: Vec<Value>,
    operation: OperationKind,
) -> ApiResult<ExecuteResponse> {
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
async fn version(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    operation: OperationKind,
) -> ApiResult<()> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Role memberships require PostgreSQL",
        )
        .into());
    }
    let response = query(
        store,
        session,
        connection,
        "SELECT pg_catalog.current_setting('server_version_num')::text",
        vec![],
        operation,
    )
    .await?;
    let version = response
        .rows
        .first()
        .and_then(|r| r.values.first())
        .ok_or_else(invalid_shape)?;
    if required(version)?
        .parse::<u32>()
        .map_err(|_| invalid_shape())?
        < 160000
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Explicit role membership options require PostgreSQL 16 or newer",
        )
        .into());
    }
    Ok(())
}
pub async fn memberships(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresObjectPageQuery,
) -> ApiResult<PostgresObjectPage<PostgresRoleMembership>> {
    version(
        store,
        session,
        connection,
        OperationKind::ListPostgresObjects,
    )
    .await?;
    let (rows, limit) = read(store, session, connection, query.clone(), LIST).await?;
    page(rows, query.offset, limit, |values| {
        if values.len() != 6 {
            return Err(invalid_shape());
        }
        Ok(PostgresRoleMembership {
            role: required(&values[0])?,
            member: required(&values[1])?,
            grantor: required(&values[2])?,
            admin_option: boolean(&values[3])?,
            inherit_option: boolean(&values[4])?,
            set_option: boolean(&values[5])?,
        })
    })
}
#[derive(serde::Deserialize)]
struct Edge {
    oid: String,
    grantor: String,
    admin_option: bool,
    inherit_option: bool,
    set_option: bool,
}
fn edges(value: &Value) -> ApiResult<Vec<Edge>> {
    let edges: Vec<Edge> = serde_json::from_str(&required(value)?).map_err(|_| invalid_shape())?;
    if edges.len() > 200 {
        return Err(ApiError::BadRequest(
            "Membership review exceeds 200 native grant edges".into(),
        ));
    }
    if edges
        .iter()
        .any(|edge| edge.oid.is_empty() || edge.grantor.is_empty())
    {
        return Err(invalid_shape());
    }
    Ok(edges)
}
pub(super) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    let (role, member, grantor, options) = match &action {
        PostgresObjectAction::GrantRoleMembership {
            role,
            member,
            grantor,
            admin_option,
            inherit_option,
            set_option,
        } => (
            role,
            member,
            grantor,
            Some((*admin_option, *inherit_option, *set_option)),
        ),
        PostgresObjectAction::RevokeRoleMembership {
            role,
            member,
            grantor,
        } => (role, member, grantor, None),
        _ => unreachable!("membership actions only"),
    };
    let (role, member, grantor) = (identifier(role)?, identifier(member)?, identifier(grantor)?);
    if role == member {
        return Err(ApiError::BadRequest(
            "A role cannot be its own member".into(),
        ));
    }
    version(
        store,
        session,
        connection,
        OperationKind::PreviewPostgresObject,
    )
    .await?;
    let response = query(
        store,
        session,
        connection,
        STATE,
        vec![
            Value::Text(role.into()),
            Value::Text(member.into()),
            Value::Text(grantor.into()),
        ],
        OperationKind::PreviewPostgresObject,
    )
    .await?;
    let state = &response
        .rows
        .first()
        .ok_or_else(|| ApiError::BadRequest("Role, member or grantor was not found".into()))?
        .values;
    if state.len() != 8 {
        return Err(invalid_shape());
    }
    if !boolean(&state[0])? || !boolean(&state[1])? {
        return Err(ApiError::Forbidden("The actor must possess the explicit grantor's privileges and the grantor must have native ADMIN authority".into()));
    }
    let pair = edges(&state[2])?;
    let _authority = edges(&state[3])?;
    let grantor_oid = required(&state[7])?;
    let existing = pair.iter().find(|edge| edge.grantor == grantor_oid);
    let sql = if let Some((admin, inherit, set)) = options {
        if existing.is_some_and(|edge| {
            (edge.admin_option, edge.inherit_option, edge.set_option) == (admin, inherit, set)
        }) {
            return Err(ApiError::BadRequest(
                "This grantor's membership already has the requested options".into(),
            ));
        }
        format!(
            "GRANT {} TO {} WITH ADMIN {}, INHERIT {}, SET {} GRANTED BY {}",
            quote(role),
            quote(member),
            admin,
            inherit,
            set,
            quote(grantor)
        )
    } else {
        if existing.is_none() {
            return Err(ApiError::BadRequest(
                "This grantor has no explicit membership to revoke".into(),
            ));
        }
        format!(
            "REVOKE {} FROM {} GRANTED BY {} RESTRICT",
            quote(role),
            quote(member),
            quote(grantor)
        )
    };
    let mut digest = Sha256::new();
    digest.update(sql.as_bytes());
    digest.update(serde_json::to_vec(state).map_err(|_| invalid_shape())?);
    let warning=format!("Role membership changes authorization across the PostgreSQL cluster. ADMIN permits membership administration; INHERIT and SET can confer role privileges. Other grantors can preserve effective membership ({} other explicit grants). RESTRICT refuses dependent revocations; native permission and cycle checks may still reject apply. Existing sessions and effective object privileges are not fully modeled.",pair.iter().filter(|edge|edge.grantor!=grantor_oid).count());
    Ok(PostgresObjectPreview {
        action,
        sql,
        precondition: format!("{:x}", digest.finalize()),
        warning,
        impact: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_review_refuses_incomplete_bounded_edge_sets() {
        let make = |count: usize| {
            Value::Text(serde_json::to_string(&(0..count).map(|i|serde_json::json!({"oid":i.to_string(),"grantor":"10","admin_option":true,"inherit_option":true,"set_option":true})).collect::<Vec<_>>()).unwrap())
        };
        assert_eq!(edges(&make(200)).unwrap().len(), 200);
        assert!(matches!(edges(&make(201)), Err(ApiError::BadRequest(_))));
    }
}
