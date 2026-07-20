//! Execution plans (Phase D).
//!
//! Captures a query's plan and normalizes it into the engine-neutral
//! [`PlanNode`] tree. Postgres `EXPLAIN (FORMAT JSON)` parses via `serde_json`;
//! SQL Server showplan XML parses via `roxmltree`. Composes over
//! `SessionStore::execute_http` + the transaction path — no new `Driver`
//! method (ADR-017 preserved). See `docs/PLANS/execution-plans.md` (ADR-025).
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
    let engine = store.conn_entry(session_id, conn_id)?.driver.engine();
    match engine {
        Engine::Postgres => explain_pg(store, session_id, conn_id, req).await,
        Engine::SqlServer => explain_mssql(store, session_id, conn_id, req).await,
    }
}

async fn explain_pg(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    req: &ExplainRequest,
) -> ApiResult<ExplainResponse> {
    let prefix = if req.analyze {
        "EXPLAIN (ANALYZE true, FORMAT JSON) "
    } else {
        "EXPLAIN (FORMAT JSON) "
    };
    let sql = format!("{prefix}{}", req.sql);

    let resp = if req.analyze && !is_plain_read(&req.sql) {
        // Running the statement for real would commit side effects; wrap in a
        // transaction that always rolls back.
        let mut rows =
            run_seq_rollback(store, session_id, conn_id, vec![(sql, req.params.clone())]).await?;
        rows.pop()
            .ok_or_else(|| ApiError::Internal("EXPLAIN produced no result".into()))?
    } else {
        store
            .execute_http(session_id, exec(conn_id, sql, req.params.clone(), None))
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
    // `SET SHOWPLAN_XML ON` must be its own batch; once on, the next statement
    // returns its plan XML instead of executing. Turn it back off afterwards so
    // the (single-session) connection returns data again.
    store
        .execute_http(
            session_id,
            exec(conn_id, "SET SHOWPLAN_XML ON".into(), vec![], None),
        )
        .await?;
    let plan_resp = store
        .execute_http(
            session_id,
            exec(conn_id, req.sql.clone(), req.params.clone(), None),
        )
        .await;
    let off = store
        .execute_http(
            session_id,
            exec(conn_id, "SET SHOWPLAN_XML OFF".into(), vec![], None),
        )
        .await;
    if let Err(e) = off {
        tracing::warn!(error = %e, "failed to disable SHOWPLAN_XML after explain");
    }
    let plan_resp = plan_resp?;

    let xml = first_text(&plan_resp).ok_or_else(|| {
        ApiError::Driver(
            DriverError::new(Code::DriverInternal, "SHOWPLAN_XML returned no plan")
                .with_engine(Engine::SqlServer),
        )
    })?;
    let root = parse_mssql_plan(&xml).map_err(ApiError::Driver)?;
    Ok(ExplainResponse {
        engine: Engine::SqlServer,
        analyzed: false,
        root,
        raw: xml,
        warnings: Vec::new(),
    })
}

/// Run each statement under one transaction, then always roll back. Returns the
/// per-statement responses. Used for ANALYZE of a mutating statement.
async fn run_seq_rollback(
    store: &SessionStore,
    session_id: SessionId,
    conn_id: ConnectionId,
    stmts: Vec<(String, Vec<Value>)>,
) -> ApiResult<Vec<ExecuteResponse>> {
    let info = store
        .begin_transaction(
            session_id,
            BeginTransactionRequest {
                connection: conn_id,
                mode: TxMode::default(),
            },
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
            .execute_http(session_id, exec(conn_id, sql, params, Some(tx.clone())))
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
        .rollback_transaction(
            session_id,
            EndTransactionRequest {
                connection: conn_id,
                tx_id: tx.tx_id,
            },
        )
        .await
    {
        tracing::warn!(error = %e, "rollback after EXPLAIN ANALYZE failed");
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
    }
}

/// A statement whose leading keyword makes it a guaranteed read (no side
/// effects). Anything else is wrapped in a rolled-back transaction for ANALYZE.
fn is_plain_read(sql: &str) -> bool {
    let kw: String = sql
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(kw.as_str(), "SELECT" | "SHOW" | "VALUES" | "TABLE")
}

fn first_json(resp: &ExecuteResponse) -> Option<serde_json::Value> {
    match resp.rows.first()?.values.first()? {
        Value::Json(v) => Some(v.clone()),
        Value::Text(s) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

fn first_text(resp: &ExecuteResponse) -> Option<String> {
    match resp.rows.first()?.values.first()? {
        Value::Text(s) => Some(s.clone()),
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
    fn is_plain_read_classifies() {
        assert!(is_plain_read("SELECT * FROM t"));
        assert!(is_plain_read("  select 1"));
        assert!(!is_plain_read("INSERT INTO t VALUES (1)"));
        assert!(!is_plain_read(
            "WITH x AS (...) INSERT INTO t SELECT * FROM x"
        ));
        assert!(!is_plain_read("UPDATE t SET a = 1"));
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
}
