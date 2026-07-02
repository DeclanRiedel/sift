//! Query execution path: take conn, register cancel token, spawn task that
//! streams pages, restore slot on completion.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use sift_driver_api::{ConnHandle, ResultSetStream};
use sift_protocol::{Code, CursorId, DriverError, DriverWarning, ExecuteRequest, Page, Row, Value};
use tokio::sync::mpsc;
use tokio_postgres::types::ToSql;
use tokio_postgres::SimpleQueryMessage;

use crate::conn::{PgDriverInner, PooledConn, SlotKind};
use crate::decode::{col_to_metadata, PgValue};
use crate::{pg_err, PgDriver};

const ROW_BATCH_SIZE: usize = 128;

struct QueryJob {
    inner: Arc<PgDriverInner>,
    conn_id: u64,
    slot_kind: SlotKind,
    conn: PooledConn,
    cursor_id_num: u64,
    page_tx: mpsc::Sender<Page>,
    sql: String,
    params: Vec<Value>,
}

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
    // Keep one page ahead at most. Combined with batched rows this ties
    // driver-side production to HTTP/WS consumption without buffering large
    // result sets in memory.
    let (page_tx, page_rx) = mpsc::channel::<Page>(1);

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
    let ExecuteRequest { sql, params } = req;

    tokio::spawn(run_query(QueryJob {
        inner,
        conn_id,
        slot_kind,
        conn,
        cursor_id_num,
        page_tx,
        sql,
        params,
    }));

    Ok(ResultSetStream::with_cursor_mode(cursor_id, page_rx, false))
}

async fn run_query(job: QueryJob) {
    // Catch panics so a panicking decode path produces a Page::Done with a
    // diagnostic instead of silently dropping the channel.
    let cursor_id_num = job.cursor_id_num;
    let page_tx = job.page_tx.clone();
    let fut = AssertUnwindSafe(run_query_inner(job));
    match fut.catch_unwind().await {
        Ok(()) => {}
        Err(panic) => {
            let msg = panic_message(panic);
            tracing::error!(cursor_id_num, "query task panicked: {msg}");
            let _ = page_tx
                .send(Page::Error {
                    error: DriverError::new(
                        Code::DriverInternal,
                        format!("query task panicked: {msg}"),
                    ),
                })
                .await;
        }
    }
}

async fn run_query_inner(job: QueryJob) {
    // Dispatch on leading keyword. SELECT/WITH/TABLE/VALUES/SHOW/EXPLAIN are
    // the row-producing statements in PG; route them through the streaming
    // extended-protocol path. Everything else (INSERT/UPDATE/DELETE/DDL/
    // utility) goes through `simple_query`, which gives us the affected-row
    // count via `CommandComplete` at the cost of materialising text-format
    // rows — acceptable because these statements rarely return large rowsets.
    if !job.params.is_empty() || is_row_producing(&job.sql) {
        run_streaming(job).await;
    } else {
        run_simple(job).await;
    }
}

/// Stream rows via `query_raw`. Used for SELECT-class statements.
async fn run_streaming(job: QueryJob) {
    let QueryJob {
        inner,
        conn_id,
        slot_kind,
        conn,
        cursor_id_num,
        page_tx,
        sql,
        params,
    } = job;

    let param_boxes = match params_to_pg(params) {
        Ok(p) => p,
        Err(error) => {
            let _ = page_tx.send(Page::Error { error }).await;
            finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
            return;
        }
    };
    let param_refs: Vec<&(dyn ToSql + Sync)> = param_boxes
        .iter()
        .map(|p| p.as_ref() as &(dyn ToSql + Sync))
        .collect();
    let stream = match conn.query_raw(&sql, param_refs).await {
        Ok(s) => s,
        Err(e) => {
            let err = pg_err(e);
            let _ = page_tx.send(Page::Error { error: err }).await;
            finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
            return;
        }
    };

    tokio::pin!(stream);

    let mut warnings = Vec::new();
    let mut columns_sent = false;
    let mut row_batch = Vec::with_capacity(ROW_BATCH_SIZE);

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
                row_batch.push(Row::new(values));
                if row_batch.len() >= ROW_BATCH_SIZE {
                    let batch = std::mem::take(&mut row_batch);
                    if page_tx.send(Page::Rows(batch)).await.is_err() {
                        finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
                        return;
                    }
                }
            }
            Err(e) => {
                warnings.push(DriverWarning::new(e.to_string()));
            }
        }
    }

    if !row_batch.is_empty() {
        let _ = page_tx.send(Page::Rows(row_batch)).await;
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

    finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
}

/// Run via `simple_query` to capture affected-row counts. Used for DML/DDL.
async fn run_simple(job: QueryJob) {
    let QueryJob {
        inner,
        conn_id,
        slot_kind,
        conn,
        cursor_id_num,
        page_tx,
        sql,
        params: _,
    } = job;

    let messages = match conn.simple_query(&sql).await {
        Ok(m) => m,
        Err(e) => {
            let err = pg_err(e);
            let _ = page_tx.send(Page::Error { error: err }).await;
            finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
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
                    let value = row
                        .get(i)
                        .map(|v| Value::Text(v.to_owned()))
                        .unwrap_or(Value::Null);
                    values.push(value);
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

    finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
}

fn params_to_pg(params: Vec<Value>) -> Result<Vec<Box<dyn ToSql + Sync + Send>>, DriverError> {
    let mut out: Vec<Box<dyn ToSql + Sync + Send>> = Vec::with_capacity(params.len());
    for value in params {
        let param: Box<dyn ToSql + Sync + Send> = match value {
            Value::Null => Box::new(None::<String>),
            Value::Bool(v) => Box::new(v),
            Value::Int16(v) => Box::new(v),
            Value::Int32(v) => Box::new(v),
            Value::Int64(v) => Box::new(v),
            Value::Float32(v) => Box::new(v),
            Value::Float64(v) => Box::new(v),
            Value::Decimal(v) => Box::new(v),
            Value::Text(v) => Box::new(v),
            Value::Blob(v) => Box::new(v),
            Value::Date(v) => Box::new(v),
            Value::Time(v) => Box::new(v),
            Value::Timestamp(v) => Box::new(v),
            Value::TimestampTz(v) => Box::new(v),
            Value::Uuid(v) => Box::new(v),
            Value::Json(v) => Box::new(v),
            Value::Interval(_) | Value::Engine { .. } => {
                return Err(DriverError::new(
                    Code::UnsupportedForEngine,
                    "parameter type is not supported by Postgres driver yet",
                )
                .with_engine(sift_protocol::Engine::Postgres));
            }
        };
        out.push(param);
    }
    Ok(out)
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
