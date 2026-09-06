//! Execution plans.
//!
//! Captures a query's plan and normalizes it into the engine-neutral
//! [`PlanNode`] tree. Postgres `EXPLAIN (FORMAT JSON)` parses via `serde_json`;
//! SQL Server showplan XML parses via `roxmltree`. Composes over
//! `SessionStore::execute_http` + the transaction path — no new `Driver`
//! method (ADR-017 preserved).
//!
//! ANALYZE safety: for a statement that is not a plain read, `analyze=true`
//! runs inside a transaction that always rolls back, so DML side effects are
//! discarded. SQL Server ANALYZE (STATISTICS XML) is not wired in v1.

use sift_protocol::{
    BeginTransactionRequest, Code, ConnectionId, DriverError, EndTransactionRequest, Engine,
    ExecuteRequestHttp, ExecuteResponse, ExplainRequest, ExplainResponse, PlanNode, SessionId,
    TxHandleRef, TxMode, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

/// Capture and normalize the execution plan for `req.sql`.
pub async fn explain(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    req: &ExplainRequest,
) -> ApiResult<ExplainResponse> {
    explain_as(
        store,
        session_id,
        conn_id,
        req,
        sift_protocol::OperationKind::Explain,
    )
    .await
}

pub(crate) async fn explain_as(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    req: &ExplainRequest,
    operation: sift_protocol::OperationKind,
) -> ApiResult<ExplainResponse> {
    store.authorize_connection_operation(session_id, conn_id, operation, Some(&req.sql), &[])?;

    let engine = store.conn_entry(session_id, conn_id)?.driver.engine();
    validate_explain_sql(engine, &req.sql)?;
    match engine {
        Engine::Postgres => explain_pg(store, session_id, conn_id, req, operation).await,
        Engine::SqlServer => explain_mssql(store, session_id, conn_id, req, operation).await,
    }
}

fn validate_explain_sql(engine: Engine, sql: &str) -> ApiResult<()> {
    let dialect: Box<dyn sqlparser::dialect::Dialect> = match engine {
        Engine::Postgres => Box::new(sqlparser::dialect::PostgreSqlDialect {}),
        Engine::SqlServer => Box::new(sqlparser::dialect::MsSqlDialect {}),
    };
    let statements = sqlparser::parser::Parser::parse_sql(dialect.as_ref(), sql).map_err(|_| {
        ApiError::BadRequest("explain requires a supported single query or DML statement".into())
    })?;
    use sqlparser::ast::Statement;
    if statements.len() != 1
        || !matches!(
            statements.first(),
            Some(
                Statement::Query(_)
                    | Statement::Insert(_)
                    | Statement::Update { .. }
                    | Statement::Delete(_)
                    | Statement::Merge { .. }
            )
        )
    {
        return Err(ApiError::BadRequest(
            "explain requires a single query or DML statement".into(),
        ));
    }

    Ok(())
}

async fn explain_pg(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    req: &ExplainRequest,
    operation: sift_protocol::OperationKind,
) -> ApiResult<ExplainResponse> {
    let prefix = if req.analyze {
        "EXPLAIN (ANALYZE true, FORMAT JSON) "
    } else {
        "EXPLAIN (FORMAT JSON) "
    };
    let sql = format!("{prefix}{}", req.sql);

    let resp = if req.analyze {
        // Running the statement for real would commit side effects; wrap in a
        // transaction that always rolls back.
        let mut rows = run_seq_rollback(
            store,
            session_id,
            conn_id,
            vec![(sql, req.params.clone())],
            operation,
        )
        .await?;
        rows.pop()
            .ok_or_else(|| ApiError::Internal("EXPLAIN produced no result".into()))?
    } else {
        store
            .execute_http_as(
                session_id,
                exec(conn_id, sql, req.params.clone(), None),
                operation,
            )
            .await?
    };

    let json = first_json(&resp).ok_or_else(|| {
        ApiError::Driver(
            DriverError::new(Code::DriverInternal, "EXPLAIN returned no plan row")
                .with_engine(Engine::Postgres),
        )
    })?;
    let root = parse_pg_plan(&json).map_err(ApiError::Driver)?;
    let raw = serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string());
    Ok(ExplainResponse {
        engine: Engine::Postgres,
        analyzed: req.analyze,
        root,
        raw,
        warnings: Vec::new(),
    })
}

async fn explain_mssql(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    req: &ExplainRequest,
    _operation: sift_protocol::OperationKind,
) -> ApiResult<ExplainResponse> {
    if req.analyze {
        return Err(ApiError::Driver(
            DriverError::new(
                Code::UnsupportedForEngine,
                "EXPLAIN ANALYZE is not yet supported for SQL Server; \
                 request analyze=false for an estimated plan",
            )
            .with_engine(Engine::SqlServer),
        ));
    }
    store.validate_execute_tx(session_id, conn_id, None)?;
    let sql = mssql_plan_batch(&req.sql, &req.params)?;
    let entry = store.conn_entry(session_id, conn_id)?;
    let driver = entry
        .driver
        .legacy_driver()
        .cloned()
        .ok_or_else(|| ApiError::BadRequest("native SQL Server provider required".into()))?;
    let handle = entry
        .handle
        .builtin()
        .cloned()
        .ok_or_else(|| ApiError::BadRequest("native SQL Server connection required".into()))?;
    let resources = store.reserve_query_resources(&entry)?;
    let (_, max_bytes) = store.result_limits();
    let captured = store
        .run_bounded("estimated plan", async move {
            let _resources = resources;
            driver
                .as_mssql()
                .ok_or_else(|| {
                    DriverError::new(
                        Code::UnsupportedForEngine,
                        "SQL Server plan backend unavailable",
                    )
                })?
                .estimated_plan(handle, sql)
                .await
        })
        .await;
    let xml = match captured {
        Ok(xml) => xml,
        Err(error) => {
            let _ = store.close_connection(session_id, conn_id).await;
            return Err(error);
        }
    };
    if xml.len() > max_bytes {
        return Err(ApiError::Driver(DriverError::new(
            Code::ResultTooLarge,
            "estimated plan exceeds result byte limit",
        )));
    }
    let root = parse_mssql_plan(&xml).map_err(ApiError::Driver)?;
    Ok(ExplainResponse {
        engine: Engine::SqlServer,
        analyzed: false,
        root,
        raw: xml,
        warnings: if req.params.is_empty() {
            Vec::new()
        } else {
            vec![sift_protocol::DriverWarning::new(
                "Estimated plan uses declared local parameter types, not sniffed runtime values",
            )]
        },
    })
}

// SHOWPLAN does not execute the sp_executesql RPC wrapper used for binds.
// Declare local variables in a SQL batch; never interpolate parameter values.
fn mssql_plan_batch(sql: &str, params: &[Value]) -> ApiResult<String> {
    use sqlparser::{dialect::MsSqlDialect, parser::Parser, tokenizer::Token};
    let mut batch = String::new();
    for (index, value) in params.iter().enumerate() {
        let type_name = match value {
            Value::Bool(_) => "bit",
            Value::Int16(_) => "smallint",
            Value::Int32(_) => "int",
            Value::Int64(_) => "bigint",
            Value::Float32(_) => "real",
            Value::Float64(_) => "float",
            Value::Text(_) | Value::Decimal(_) | Value::Json(_) => "nvarchar(max)",
            Value::Blob(_) => "varbinary(max)",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::Timestamp(_) => "datetime2",
            Value::TimestampTz(_) => "datetimeoffset",
            Value::Uuid(_) => "uniqueidentifier",
            Value::TypedNull { type_name } => {
                if type_name.len() > 256 {
                    return Err(ApiError::BadRequest("invalid plan parameter type".into()));
                }
                let mut parser = Parser::new(&MsSqlDialect {})
                    .try_with_sql(type_name)
                    .map_err(|_| ApiError::BadRequest("invalid plan parameter type".into()))?;
                let parsed = parser
                    .parse_data_type()
                    .map_err(|_| ApiError::BadRequest("invalid plan parameter type".into()))?;
                if parser.peek_token().token != Token::EOF {
                    return Err(ApiError::BadRequest("invalid plan parameter type".into()));
                }
                batch.push_str(&format!("DECLARE @P{} {};\n", index + 1, parsed));
                continue;
            }
            Value::Null | Value::Interval(_) | Value::Native { .. } => {
                return Err(ApiError::Driver(DriverError::new(
                    Code::UnsupportedForEngine,
                    "SQL Server plan parameters require supported values or explicit typed NULLs",
                )));
            }
        };
        batch.push_str(&format!("DECLARE @P{} {type_name};\n", index + 1));
    }
    batch.push_str(sql);
    Ok(batch)
}

/// Run each statement under one transaction, then always roll back. Returns the
/// per-statement responses. Used for ANALYZE of a mutating statement.
async fn run_seq_rollback(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    stmts: Vec<(String, Vec<Value>)>,
    operation: sift_protocol::OperationKind,
) -> ApiResult<Vec<ExecuteResponse>> {
    let info = store
        .begin_transaction_as(
            session_id,
            BeginTransactionRequest {
                connection: conn_id,
                mode: TxMode::default(),
            },
            operation,
        )
        .await?;
    let tx = TxHandleRef {
        tx_id: info.tx_id,
        connection: info.connection,
        mode: info.mode,
    };
    let mut out = Vec::with_capacity(stmts.len());
    let mut failure = None;
    for (sql, params) in stmts {
        match store
            .execute_http_as(
                session_id,
                exec(conn_id, sql, params, Some(tx.clone())),
                operation,
            )
            .await
        {
            Ok(r) => out.push(r),
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    // Always roll back — the plan is captured, the mutation is discarded.
    if let Err(e) = store
        .rollback_transaction_as(
            session_id,
            EndTransactionRequest {
                connection: conn_id,
                tx_id: tx.tx_id,
            },
            operation,
        )
        .await
    {
        let _ = store.close_connection(session_id, conn_id).await;
        return Err(e);
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

fn exec(
    conn_id: ConnectionId,
    sql: String,
    params: Vec<Value>,
    tx: Option<TxHandleRef>,
) -> ExecuteRequestHttp {
    ExecuteRequestHttp {
        connection: conn_id,
        sql,
        params,
        tx,
        room_id: None,
        connection_profile_id: None,
        transform: None,
        source: None,
    }
}

pub fn compare_plan_captures(
    left: &sift_protocol::PlanCapture,
    right: &sift_protocol::PlanCapture,
    max_changes: usize,
) -> ApiResult<sift_protocol::PlanCaptureComparison> {
    if left.engine != right.engine {
        return Err(ApiError::BadRequest(
            "normalized plan costs cannot be compared across engines".into(),
        ));
    }
    if max_changes == 0 || max_changes > 10_000 {
        return Err(ApiError::BadRequest(
            "plan comparison change limit must be between 1 and 10000".into(),
        ));
    }
    let mut comparison = sift_protocol::PlanCaptureComparison {
        left: left.id,
        right: right.id,
        engine: left.engine,
        operator_changes: 0,
        cardinality_changes: 0,
        cost_changes: 0,
        runtime_changes: 0,
        changes: Vec::new(),
        truncated: false,
    };
    compare_plan_nodes(
        Some(&left.root),
        Some(&right.root),
        &mut Vec::new(),
        &mut comparison,
        max_changes,
    );
    Ok(comparison)
}

fn compare_plan_nodes(
    left: Option<&PlanNode>,
    right: Option<&PlanNode>,
    path: &mut Vec<u32>,
    comparison: &mut sift_protocol::PlanCaptureComparison,
    max_changes: usize,
) {
    use sift_protocol::{PlanChangeKind, PlanNodeChange};

    let change = match (left, right) {
        (Some(left), Some(right)) => {
            let operator_changed = left.op != right.op || left.relation != right.relation;
            let cardinality_changed = left.est_rows != right.est_rows;
            let cost_changed = left.est_cost != right.est_cost;
            let runtime_changed =
                left.actual_rows != right.actual_rows || left.actual_ms != right.actual_ms;
            comparison.operator_changes += u32::from(operator_changed);
            comparison.cardinality_changes += u32::from(cardinality_changed);
            comparison.cost_changes += u32::from(cost_changed);
            comparison.runtime_changes += u32::from(runtime_changed);
            (operator_changed || cardinality_changed || cost_changed || runtime_changed).then(
                || PlanNodeChange {
                    path: path.clone(),
                    kind: PlanChangeKind::Modified,
                    left_operator: Some(left.op.clone()),
                    right_operator: Some(right.op.clone()),
                    estimated_rows_delta: delta(left.est_rows, right.est_rows),
                    estimated_rows_ratio: ratio(left.est_rows, right.est_rows),
                    estimated_cost_delta: delta(left.est_cost, right.est_cost),
                    actual_rows_delta: delta(left.actual_rows, right.actual_rows),
                    actual_ms_delta: delta(left.actual_ms, right.actual_ms),
                },
            )
        }
        (Some(left), None) => {
            comparison.operator_changes += 1;
            Some(PlanNodeChange {
                path: path.clone(),
                kind: PlanChangeKind::Removed,
                left_operator: Some(left.op.clone()),
                right_operator: None,
                estimated_rows_delta: None,
                estimated_rows_ratio: None,
                estimated_cost_delta: None,
                actual_rows_delta: None,
                actual_ms_delta: None,
            })
        }
        (None, Some(right)) => {
            comparison.operator_changes += 1;
            Some(PlanNodeChange {
                path: path.clone(),
                kind: PlanChangeKind::Added,
                left_operator: None,
                right_operator: Some(right.op.clone()),
                estimated_rows_delta: None,
                estimated_rows_ratio: None,
                estimated_cost_delta: None,
                actual_rows_delta: None,
                actual_ms_delta: None,
            })
        }
        (None, None) => None,
    };
    if let Some(change) = change {
        if comparison.changes.len() < max_changes {
            comparison.changes.push(change);
        } else {
            comparison.truncated = true;
        }
    }
    let child_count = left
        .map_or(0, |node| node.children.len())
        .max(right.map_or(0, |node| node.children.len()));
    for index in 0..child_count {
        path.push(index as u32);
        compare_plan_nodes(
            left.and_then(|node| node.children.get(index)),
            right.and_then(|node| node.children.get(index)),
            path,
            comparison,
            max_changes,
        );
        path.pop();
    }
}

fn delta(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    left.zip(right).map(|(left, right)| right - left)
}

fn ratio(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    left.zip(right)
        .filter(|(left, right)| left.is_finite() && right.is_finite() && *left != 0.0)
        .map(|(left, right)| right / left)
}

fn first_json(resp: &ExecuteResponse) -> Option<serde_json::Value> {
    match resp.rows.first()?.values.first()? {
        Value::Json(v) => Some(v.clone()),
        Value::Text(s) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

// --- Postgres EXPLAIN (FORMAT JSON) --------------------------------------

const PG_MAPPED: &[&str] = &[
    "Node Type",
    "Relation Name",
    "Index Name",
    "Plan Rows",
    "Total Cost",
    "Actual Rows",
    "Actual Total Time",
    "Plans",
];

fn parse_pg_plan(v: &serde_json::Value) -> Result<PlanNode, DriverError> {
    let plan = v
        .as_array()
        .and_then(|a| a.first())
        .and_then(|o| o.get("Plan"))
        .ok_or_else(|| {
            DriverError::new(Code::DriverInternal, "unexpected EXPLAIN JSON shape")
                .with_engine(Engine::Postgres)
        })?;
    Ok(pg_node(plan))
}

fn pg_node(o: &serde_json::Value) -> PlanNode {
    let Some(obj) = o.as_object() else {
        return PlanNode::new("Unknown");
    };
    let mut node = PlanNode::new(
        obj.get("Node Type")
            .and_then(|x| x.as_str())
            .unwrap_or("Unknown"),
    );
    node.relation = obj
        .get("Relation Name")
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("Index Name").and_then(|x| x.as_str()))
        .map(str::to_string);
    node.est_rows = obj.get("Plan Rows").and_then(|x| x.as_f64());
    node.est_cost = obj.get("Total Cost").and_then(|x| x.as_f64());
    node.actual_rows = obj.get("Actual Rows").and_then(|x| x.as_f64());
    node.actual_ms = obj.get("Actual Total Time").and_then(|x| x.as_f64());
    if let Some(plans) = obj.get("Plans").and_then(|x| x.as_array()) {
        node.children = plans.iter().map(pg_node).collect();
    }
    for (k, val) in obj {
        if !PG_MAPPED.contains(&k.as_str()) {
            node.extra.insert(k.clone(), val.clone());
        }
    }
    node
}

// --- SQL Server showplan XML ---------------------------------------------

const MSSQL_MAPPED: &[&str] = &["PhysicalOp", "EstimateRows", "EstimatedTotalSubtreeCost"];

fn is_tag(n: roxmltree::Node, name: &str) -> bool {
    // Match by local name so the showplan default namespace doesn't matter.
    n.tag_name().name() == name
}

fn nearest_relop(d: roxmltree::Node) -> Option<roxmltree::NodeId> {
    d.ancestors()
        .skip(1)
        .find(|a| is_tag(*a, "RelOp"))
        .map(|a| a.id())
}

fn parse_mssql_plan(xml: &str) -> Result<PlanNode, DriverError> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| {
        DriverError::new(
            Code::DriverInternal,
            format!("showplan XML parse failed: {e}"),
        )
        .with_engine(Engine::SqlServer)
    })?;
    let relop = doc
        .descendants()
        .find(|n| is_tag(*n, "RelOp"))
        .ok_or_else(|| {
            DriverError::new(Code::DriverInternal, "no RelOp in showplan XML")
                .with_engine(Engine::SqlServer)
        })?;
    Ok(mssql_node(relop))
}

fn mssql_node(node: roxmltree::Node) -> PlanNode {
    let mut p = PlanNode::new(node.attribute("PhysicalOp").unwrap_or("Unknown"));
    p.est_rows = node.attribute("EstimateRows").and_then(|s| s.parse().ok());
    p.est_cost = node
        .attribute("EstimatedTotalSubtreeCost")
        .and_then(|s| s.parse().ok());
    p.relation = node
        .descendants()
        .find(|d| is_tag(*d, "Object") && nearest_relop(*d) == Some(node.id()))
        .and_then(|o| o.attribute("Table").or_else(|| o.attribute("Index")))
        .map(|s| s.trim_matches(['[', ']']).to_string());

    // Actual counters (present only for STATISTICS XML / actual plans).
    let counters: Vec<_> = node
        .descendants()
        .filter(|d| is_tag(*d, "RunTimeCountersPerThread") && nearest_relop(*d) == Some(node.id()))
        .collect();
    if !counters.is_empty() {
        p.actual_rows = Some(
            counters
                .iter()
                .filter_map(|c| {
                    c.attribute("ActualRows")
                        .and_then(|s| s.parse::<f64>().ok())
                })
                .sum(),
        );
        p.actual_ms = counters
            .iter()
            .filter_map(|c| {
                c.attribute("ActualElapsedms")
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .fold(None, |acc, v| Some(acc.map_or(v, |a: f64| a.max(v))));
    }

    for a in node.attributes() {
        if !MSSQL_MAPPED.contains(&a.name()) {
            p.extra.insert(
                a.name().to_string(),
                serde_json::Value::String(a.value().to_string()),
            );
        }
    }

    p.children = node
        .descendants()
        .filter(|d| {
            is_tag(*d, "RelOp") && d.id() != node.id() && nearest_relop(*d) == Some(node.id())
        })
        .map(mssql_node)
        .collect();
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_rejects_batches_and_session_controls() {
        for engine in [Engine::Postgres, Engine::SqlServer] {
            assert!(validate_explain_sql(engine, "SELECT 1; DELETE FROM users").is_err());
            assert!(validate_explain_sql(engine, "SET SHOWPLAN_XML OFF").is_err());
            assert!(validate_explain_sql(engine, "SELECT ';' AS value").is_ok());
        }
    }

    #[test]
    fn mssql_plan_declarations_never_interpolate_values_or_type_statements() {
        let sql = mssql_plan_batch("SELECT @P1", &[Value::Text("private-value'".into())]).unwrap();
        assert_eq!(sql, "DECLARE @P1 nvarchar(max);\nSELECT @P1");
        assert!(mssql_plan_batch(
            "SELECT @P1",
            &[Value::TypedNull {
                type_name: "int; DROP TABLE users".into()
            }]
        )
        .is_err());
        assert_eq!(
            mssql_plan_batch(
                "SELECT @P1",
                &[Value::TypedNull {
                    type_name: "numeric(18,4)".into()
                }]
            )
            .unwrap(),
            "DECLARE @P1 NUMERIC(18,4);\nSELECT @P1"
        );
    }

    #[test]
    fn parse_pg_plan_builds_tree() {
        let json = serde_json::json!([{
            "Plan": {
                "Node Type": "Hash Join",
                "Total Cost": 25.0,
                "Plan Rows": 50,
                "Hash Cond": "(a.id = b.id)",
                "Plans": [
                    {"Node Type": "Seq Scan", "Relation Name": "users", "Plan Rows": 100, "Total Cost": 12.5, "Filter": "(id > 5)"},
                    {"Node Type": "Hash", "Total Cost": 8.0, "Plans": [
                        {"Node Type": "Seq Scan", "Relation Name": "orders", "Plan Rows": 40, "Total Cost": 6.0}
                    ]}
                ]
            }
        }]);
        let root = parse_pg_plan(&json).unwrap();
        assert_eq!(root.op, "Hash Join");
        assert_eq!(root.est_rows, Some(50.0));
        assert_eq!(root.est_cost, Some(25.0));
        // unmapped attribute goes to extra
        assert!(root.extra.contains_key("Hash Cond"));
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].op, "Seq Scan");
        assert_eq!(root.children[0].relation.as_deref(), Some("users"));
        assert!(root.children[0].extra.contains_key("Filter"));
        // nested child under Hash
        assert_eq!(root.children[1].op, "Hash");
        assert_eq!(
            root.children[1].children[0].relation.as_deref(),
            Some("orders")
        );
    }

    #[test]
    fn parse_mssql_plan_builds_tree() {
        let xml = r#"<?xml version="1.0"?>
<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan">
  <BatchSequence><Batch><Statements><StmtSimple>
    <QueryPlan>
      <RelOp PhysicalOp="Nested Loops" EstimateRows="50" EstimatedTotalSubtreeCost="0.9" LogicalOp="Inner Join">
        <NestedLoops>
          <RelOp PhysicalOp="Clustered Index Scan" EstimateRows="100" EstimatedTotalSubtreeCost="0.5">
            <IndexScan><Object Table="[users]" Index="[PK_users]"/></IndexScan>
          </RelOp>
          <RelOp PhysicalOp="Index Seek" EstimateRows="40" EstimatedTotalSubtreeCost="0.4">
            <IndexScan><Object Table="[orders]"/></IndexScan>
          </RelOp>
        </NestedLoops>
      </RelOp>
    </QueryPlan>
  </StmtSimple></Statements></Batch></BatchSequence>
</ShowPlanXML>"#;
        let root = parse_mssql_plan(xml).unwrap();
        assert_eq!(root.op, "Nested Loops");
        assert_eq!(root.est_rows, Some(50.0));
        assert_eq!(root.est_cost, Some(0.9));
        assert!(root.extra.contains_key("LogicalOp"));
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].op, "Clustered Index Scan");
        assert_eq!(root.children[0].relation.as_deref(), Some("users"));
        assert_eq!(root.children[1].op, "Index Seek");
        assert_eq!(root.children[1].relation.as_deref(), Some("orders"));
    }

    fn capture(engine: Engine, root: PlanNode) -> sift_protocol::PlanCapture {
        sift_protocol::PlanCapture {
            id: sift_protocol::PlanCaptureId(uuid::Uuid::new_v4()),
            tenant_id: 1,
            connection_profile_id: 1,
            creator_principal_id: 1,
            provider: engine.provider_ref("test"),
            server_version: "test".into(),
            engine,
            source_digest: format!("sha256:{}", "a".repeat(64)),
            document_revision: 1,
            statement_id: "stmt:test".into(),
            statement_fingerprint: "sqlfp:test".into(),
            catalog_revision: sift_protocol::CatalogRevision(1),
            analyzed: false,
            captured_at: chrono::Utc::now(),
            duration_ms: 1,
            root,
            warnings: Vec::new(),
            complete: true,
            revision: 0,
            raw_response: None,
            source: None,
        }
    }

    #[test]
    fn normalized_plan_comparison_reports_operator_cardinality_cost_and_runtime() {
        let mut left = PlanNode::new("Seq Scan");
        left.est_rows = Some(10.0);
        left.est_cost = Some(2.0);
        left.actual_rows = Some(8.0);
        let mut right = PlanNode::new("Index Scan");
        right.est_rows = Some(20.0);
        right.est_cost = Some(1.0);
        right.actual_rows = Some(18.0);
        right.children.push(PlanNode::new("Bitmap Index Scan"));
        let comparison = compare_plan_captures(
            &capture(Engine::Postgres, left),
            &capture(Engine::Postgres, right),
            100,
        )
        .unwrap();
        assert_eq!(comparison.operator_changes, 2);
        assert_eq!(comparison.cardinality_changes, 1);
        assert_eq!(comparison.cost_changes, 1);
        assert_eq!(comparison.runtime_changes, 1);
        assert_eq!(comparison.changes.len(), 2);
        assert_eq!(comparison.changes[0].estimated_rows_ratio, Some(2.0));
    }

    #[test]
    fn normalized_plan_comparison_rejects_cross_engine_costs() {
        let error = compare_plan_captures(
            &capture(Engine::Postgres, PlanNode::new("Scan")),
            &capture(Engine::SqlServer, PlanNode::new("Scan")),
            100,
        )
        .unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }
}
