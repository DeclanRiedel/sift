//! Typed partition attachment and bounded catalog impact through the existing
//! PostgreSQL workbench preview/apply operations.
use sha2::{Digest, Sha256};
use sift_protocol::{
    Code, ConnectionId, DriverError, ExecuteRequestHttp, OperationKind, PostgresObjectAction,
    PostgresObjectImpact, PostgresObjectPreview, PostgresPartitionBound as Bound,
    PostgresPartitionValue as BoundValue, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const STATE_SQL: &str = include_str!("postgres_partitions/state.sql");

pub(crate) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: PostgresObjectAction,
) -> ApiResult<PostgresObjectPreview> {
    let PostgresObjectAction::AttachPartition {
        parent_schema,
        parent,
        child_schema,
        child,
        bound,
    } = &action
    else {
        unreachable!("attachment preview receives attachment actions")
    };
    for name in [parent_schema, parent, child_schema, child] {
        crate::postgres_workbench::identifier(name)?;
    }
    let clause = render_bound(bound)?;
    let sql = format!(
        "ALTER TABLE {}.{} ATTACH PARTITION {}.{} {clause}",
        quote(parent_schema),
        quote(parent),
        quote(child_schema),
        quote(child)
    );
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: STATE_SQL.into(),
                params: [parent_schema, parent, child_schema, child]
                    .into_iter()
                    .map(|name| Value::Text(name.clone()))
                    .collect(),
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
        .map(|row| row.values.as_slice())
        .ok_or_else(|| bad("Select an existing partitioned parent and ordinary child table"))?;
    let [Value::Text(strategy), Value::Int64(keys), Value::Bool(owned), Value::Bool(unattached), Value::Bool(columns_match), Value::Bool(has_default), Value::Text(_fingerprint), Value::Text(impact_json)] =
        state
    else {
        return Err(DriverError::new(
            Code::UnsupportedResultShape,
            "Invalid partition catalog response",
        )
        .into());
    };
    if !owned {
        return Err(ApiError::Forbidden(
            "Partition attachment requires ownership of both tables".into(),
        ));
    }
    if !unattached || !columns_match {
        return Err(bad(
            "Child must be unattached with matching native column definitions",
        ));
    }
    let compatible = match bound {
        Bound::Default => matches!(strategy.as_str(), "r" | "l") && !has_default,
        Bound::Range { from, .. } => strategy == "r" && *keys == from.len() as i64,
        Bound::List { .. } => strategy == "l" && *keys == 1,
        Bound::Hash { .. } => strategy == "h",
    };
    if !compatible {
        return Err(bad(
            "Bounds do not match the parent key or a default partition already exists",
        ));
    }
    let mut objects: Vec<String> = serde_json::from_str(impact_json).map_err(|_| {
        DriverError::new(
            Code::UnsupportedResultShape,
            "Invalid partition dependency response",
        )
    })?;
    let truncated = objects.len() > 50;
    objects.truncate(50);
    let mut digest = Sha256::new();
    digest.update(sql.as_bytes());
    digest.update(
        serde_json::to_vec(state)
            .map_err(|_| ApiError::Internal("Partition fingerprint failed".into()))?,
    );
    Ok(PostgresObjectPreview {
        action, sql, precondition: format!("{:x}", digest.finalize()),
        warning: "Attachment changes routing, may create or attach indexes and clone triggers, and can scan/lock the child and default partition. PostgreSQL validates current rows and overlap at apply; preview does not hold locks.".into(),
        impact: Some(PostgresObjectImpact { objects, truncated }),
    })
}

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}

fn quote(name: &str) -> String {
    crate::postgres_workbench::quote(name)
}

fn render_value(value: &BoundValue, range: bool) -> ApiResult<String> {
    match value {
        BoundValue::Literal(text) if text.len() <= 256 && !text.chars().any(char::is_control) => {
            Ok(format!(
                "E'{}'",
                text.replace('\\', "\\\\").replace('\'', "''")
            ))
        }
        BoundValue::Null if !range => Ok("NULL".into()),
        BoundValue::MinValue if range => Ok("MINVALUE".into()),
        BoundValue::MaxValue if range => Ok("MAXVALUE".into()),
        _ => Err(bad(
            "Invalid partition value; use bounded literals, range sentinels or list NULL",
        )),
    }
}

fn range_values(values: &[BoundValue]) -> ApiResult<String> {
    if !(1..=32).contains(&values.len()) {
        return Err(bad("Range bounds require 1..=32 key values"));
    }
    let mut sentinel = None;
    for value in values {
        let next = match value {
            BoundValue::MinValue => Some(false),
            BoundValue::MaxValue => Some(true),
            _ => None,
        };
        if sentinel.is_some() && next != sentinel {
            return Err(bad(
                "Range values after MINVALUE/MAXVALUE must use the same sentinel",
            ));
        }
        if next.is_some() {
            sentinel = next;
        }
    }
    values
        .iter()
        .map(|value| render_value(value, true))
        .collect::<ApiResult<Vec<_>>>()
        .map(|values| values.join(", "))
}

fn render_bound(bound: &Bound) -> ApiResult<String> {
    Ok(match bound {
        Bound::Default => "DEFAULT".into(),
        Bound::Range { from, to } => {
            if from.len() != to.len() {
                return Err(bad("Range endpoints must have matching key counts"));
            }
            format!(
                "FOR VALUES FROM ({}) TO ({})",
                range_values(from)?,
                range_values(to)?
            )
        }
        Bound::List { values } => {
            if !(1..=100).contains(&values.len()) {
                return Err(bad("List bounds require 1..=100 values"));
            }
            let values = values
                .iter()
                .map(|value| render_value(value, false))
                .collect::<ApiResult<Vec<_>>>()?;
            format!("FOR VALUES IN ({})", values.join(", "))
        }
        Bound::Hash { modulus, remainder }
            if (1..=i32::MAX as u32).contains(modulus) && remainder < modulus =>
        {
            format!("FOR VALUES WITH (MODULUS {modulus}, REMAINDER {remainder})")
        }
        Bound::Hash { .. } => {
            return Err(bad(
                "Hash bounds require a positive int32 modulus and a smaller remainder",
            ))
        }
    })
}

/// Native coercion errors may echo a bound literal. Keep details in the HTTP
/// response and retain only a generic message in durable operation audit.
pub(crate) fn redact_audit_error(
    operation: &sift_protocol::Operation,
    message: Option<String>,
) -> Option<String> {
    if matches!(
        operation,
        sift_protocol::Operation::PreviewPostgresObject {
            action: PostgresObjectAction::AttachPartition { .. },
            ..
        } | sift_protocol::Operation::ApplyPostgresObject {
            action: PostgresObjectAction::AttachPartition { .. },
            ..
        }
    ) {
        message.map(|_| "Partition attachment failed".into())
    } else {
        message
    }
}

/// Partition literals can contain application data and must not reach audit logs.
pub(crate) fn redact_action(action: &mut PostgresObjectAction) {
    if let PostgresObjectAction::AttachPartition { bound, .. } = action {
        match bound {
            Bound::Range { from, to } => {
                from.clear();
                to.clear();
            }
            Bound::List { values } => values.clear(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_escaped_data_and_invalid_shapes_are_refused() {
        assert_eq!(
            render_bound(&Bound::List {
                values: vec![
                    BoundValue::Literal("x'\\; DROP TABLE t; --".into()),
                    BoundValue::Null
                ]
            })
            .unwrap(),
            "FOR VALUES IN (E'x''\\\\; DROP TABLE t; --', NULL)"
        );
        assert!(render_bound(&Bound::Range {
            from: vec![],
            to: vec![]
        })
        .is_err());
        assert!(render_bound(&Bound::Range {
            from: vec![BoundValue::Null],
            to: vec![BoundValue::MaxValue]
        })
        .is_err());
        assert!(render_bound(&Bound::List {
            values: vec![BoundValue::MaxValue]
        })
        .is_err());
        assert!(render_bound(&Bound::Hash {
            modulus: 0,
            remainder: 0
        })
        .is_err());
        assert!(render_bound(&Bound::Hash {
            modulus: 4,
            remainder: 4
        })
        .is_err());
        assert!(range_values(&[BoundValue::MinValue, BoundValue::Literal("1".into())]).is_err());
        assert_eq!(
            render_bound(&Bound::Hash {
                modulus: 4,
                remainder: 3
            })
            .unwrap(),
            "FOR VALUES WITH (MODULUS 4, REMAINDER 3)"
        );
    }
}
