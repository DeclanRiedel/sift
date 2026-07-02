//! Query execution path: take conn, register cancel token, spawn task that
//! streams pages, restore slot on completion.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use sift_driver_api::{ConnHandle, ResultSetStream};
use sift_protocol::{CursorId, DriverError, DriverWarning, ExecuteRequest, Page, Row, Value};
use tokio::sync::mpsc;
use tokio_postgres::types::ToSql;
use tokio_postgres::SimpleQueryMessage;

use crate::conn::{PgDriverInner, PooledConn, SlotKind};
use crate::decode::{col_to_metadata, PgValue};
use crate::{pg_err, PgDriver};

/// Set up the streaming query: take conn, register cancel token, spawn the
/// row-pump task, return the [`ResultSetStream`] immediately.
pub(crate) async fn execute_query(
    driver: &PgDriver,
    c: ConnHandle,
    req: ExecuteRequest,
) -> Result<ResultSetStream, DriverError> {
    let (conn, slot_kind) = driver.inner.take_for_op(&c).await?;

    let cursor_id_num = driver.inner.cursor_id.next();
    let cursor_id = CursorId::new(cursor_id_num);
    let (page_tx, page_rx) = mpsc::channel::<Page>(64);

    // Register the (conn_id, cancel token) tuple before spawning so cancel()
    // racing the query's start still finds the entry, and so close() can
    // drain cursors belonging to a conn.
    let conn_id = c.id();
    let cancel_token = conn.cancel_token();
    driver
        .inner
        .cursors
        .insert(cursor_id_num, (conn_id, cancel_token));

    let inner = Arc::clone(&driver.inner);
    let sql = req.sql;

    tokio::spawn(run_query(
        inner,
        conn_id,
        slot_kind,
        conn,
        cursor_id_num,
        page_tx,
        sql,
    ));

    Ok(ResultSetStream::new(cursor_id, page_rx))
}

async fn run_query(
    inner: Arc<PgDriverInner>,
    conn_id: u64,
    slot_kind: SlotKind,
    conn: PooledConn,
    cursor_id_num: u64,
    page_tx: mpsc::Sender<Page>,
    sql: String,
) {
    // Catch panics so a panicking decode path produces a Page::Done with a
    // diagnostic instead of silently dropping the channel.
    let fut = AssertUnwindSafe(run_query_inner(
        &inner,
        conn_id,
        slot_kind,
        conn,
        cursor_id_num,
        page_tx.clone(),
        sql,
    ));
    match fut.catch_unwind().await {
        Ok(()) => {}
        Err(panic) => {
            let msg = panic_message(panic);
            tracing::error!(cursor_id_num, "query task panicked: {msg}");
            let _ = page_tx
                .send(Page::Done {
                    affected_rows: None,
                    warnings: vec![DriverWarning::new(format!("query task panicked: {msg}"))],
                })
                .await;
        }
    }
}

async fn run_query_inner(
    inner: &Arc<PgDriverInner>,
    conn_id: u64,
    slot_kind: SlotKind,
    conn: PooledConn,
    cursor_id_num: u64,
    page_tx: mpsc::Sender<Page>,
    sql: String,
) {
    // Dispatch on leading keyword. SELECT/WITH/TABLE/VALUES/SHOW/EXPLAIN are
    // the row-producing statements in PG; route them through the streaming
    // extended-protocol path. Everything else (INSERT/UPDATE/DELETE/DDL/
    // utility) goes through `simple_query`, which gives us the affected-row
    // count via `CommandComplete` at the cost of materialising text-format
    // rows — acceptable because these statements rarely return large rowsets.
    if is_row_producing(&sql) {
        run_streaming(inner, conn_id, slot_kind, conn, cursor_id_num, page_tx, sql).await;
    } else {
        run_simple(inner, conn_id, slot_kind, conn, cursor_id_num, page_tx, sql).await;
    }
}

/// Stream rows via `query_raw`. Used for SELECT-class statements.
async fn run_streaming(
    inner: &Arc<PgDriverInner>,
    conn_id: u64,
    slot_kind: SlotKind,
    conn: PooledConn,
    cursor_id_num: u64,
    page_tx: mpsc::Sender<Page>,
    sql: String,
) {
    let params: [&(dyn ToSql + Sync); 0] = [];
    let stream = match conn.query_raw(&sql, params).await {
        Ok(s) => s,
        Err(e) => {
            let err = pg_err(e);
            let _ = page_tx
                .send(Page::Done {
                    affected_rows: None,
                    warnings: vec![DriverWarning::new(err.to_string())],
                })
                .await;
            finish(inner, conn_id, slot_kind, conn, cursor_id_num).await;
            return;
        }
    };

    tokio::pin!(stream);

    let mut warnings = Vec::new();
    let mut columns_sent = false;

    while let Some(row_result) = stream.next().await {
        match row_result {
            Ok(row) => {
                if !columns_sent {
                    let cols: Vec<_> = row.columns().iter().map(col_to_metadata).collect();
                    let _ = page_tx.send(Page::NextResult { columns: cols }).await;
                    columns_sent = true;
                }
                let n = row.columns().len();
                let mut values = Vec::with_capacity(n);
                for i in 0..n {
                    values.push(decode_cell(&row, i, cursor_id_num, &mut warnings));
                }
                let _ = page_tx.send(Page::Rows(vec![Row::new(values)])).await;
            }
            Err(e) => {
                warnings.push(DriverWarning::new(e.to_string()));
            }
        }
    }

    let _ = page_tx
        .send(Page::Done {
            // Extended protocol obscures the affected-row count from us;
            // SELECT-class queries don't produce a meaningful "affected"
            // count anyway. For DML...RETURNING the caller can count rows.
            affected_rows: None,
            warnings,
        })
        .await;

    finish(inner, conn_id, slot_kind, conn, cursor_id_num).await;
}

/// Run via `simple_query` to capture affected-row counts. Used for DML/DDL.
async fn run_simple(
    inner: &Arc<PgDriverInner>,
    conn_id: u64,
    slot_kind: SlotKind,
    conn: PooledConn,
    cursor_id_num: u64,
    page_tx: mpsc::Sender<Page>,
    sql: String,
) {
    let messages = match conn.simple_query(&sql).await {
        Ok(m) => m,
        Err(e) => {
            let err = pg_err(e);
            let _ = page_tx
                .send(Page::Done {
                    affected_rows: None,
                    warnings: vec![DriverWarning::new(err.to_string())],
                })
                .await;
            finish(inner, conn_id, slot_kind, conn, cursor_id_num).await;
            return;
        }
    };

    let warnings = Vec::new();
    let mut columns_sent = false;
    let mut affected_rows: Option<u64> = None;

    for msg in messages {
        match msg {
            SimpleQueryMessage::Row(row) => {
                if !columns_sent {
                    let cols = crate::decode::simple_query_columns(&row);
                    let _ = page_tx.send(Page::NextResult { columns: cols }).await;
                    columns_sent = true;
                }
                let n = row.len();
                let mut values = Vec::with_capacity(n);
                for i in 0..n {
                    let v = row.get(i).map(str::to_owned).unwrap_or_default();
                    values.push(Value::Text(v));
                }
                let _ = page_tx.send(Page::Rows(vec![Row::new(values)])).await;
            }
            SimpleQueryMessage::CommandComplete(n) => {
                affected_rows = Some(n);
            }
            _ => {}
        }
    }

    let _ = page_tx
        .send(Page::Done {
            affected_rows,
            warnings,
        })
        .await;

    finish(inner, conn_id, slot_kind, conn, cursor_id_num).await;
}

/// Decode a single cell, surfacing decode errors as warnings + a placeholder
/// `Value::Engine` instead of silently dropping to `Null`.
fn decode_cell(
    row: &tokio_postgres::Row,
    i: usize,
    cursor_id_num: u64,
    warnings: &mut Vec<DriverWarning>,
) -> Value {
    let col_name = row.columns().get(i).map(|c| c.name().to_string());
    match row.try_get::<_, Option<PgValue>>(i) {
        Ok(Some(pv)) => pv.0,
        Ok(None) => Value::Null,
        Err(e) => {
            let msg = e.to_string();
            tracing::warn!(cursor_id_num, idx = i, "cell decode error: {msg}");
            warnings.push(DriverWarning::new(format!(
                "cell {} ({}): decode error: {msg}",
                i,
                col_name.as_deref().unwrap_or("?")
            )));
            Value::Engine {
                engine: sift_protocol::Engine::Postgres,
                type_name: "?".to_string(),
                display_text: "<decode error>".to_string(),
            }
        }
    }
}

/// True if `sql` looks like a row-producing statement (SELECT/WITH/TABLE/
/// VALUES/SHOW/EXPLAIN). Heuristic; PG has corner cases (e.g. CTEs that
/// return rows via WITH ... SELECT, or INSERT...RETURNING) that this misses.
fn is_row_producing(sql: &str) -> bool {
    let s = sql.trim_start().trim_start_matches('(').trim_start();
    let s = s
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        s.as_str(),
        "SELECT" | "WITH" | "TABLE" | "VALUES" | "SHOW" | "EXPLAIN"
    )
}

async fn finish(
    inner: &Arc<PgDriverInner>,
    conn_id: u64,
    slot_kind: SlotKind,
    conn: PooledConn,
    cursor_id_num: u64,
) {
    inner.cursors.remove(&cursor_id_num);
    inner.restore(conn_id, slot_kind, conn).await;
}

/// Extract a usable message from a panic payload.
fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = p.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}
