//! Query execution path: take conn, register cancel token, spawn task that
//! streams pages, restore slot on completion.

use std::sync::Arc;

use futures::StreamExt;
use sift_driver_api::{ConnHandle, ResultSetStream};
use sift_protocol::{CursorId, DriverError, DriverWarning, ExecuteRequest, Page, Row, Value};
use tokio::sync::mpsc;
use tokio_postgres::types::ToSql;

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

    // Register the cancel token before spawning so cancel() racing the
    // query's start still finds the entry.
    let cancel_token = conn.cancel_token();
    driver.inner.cursors.insert(cursor_id_num, cancel_token);

    let inner = Arc::clone(&driver.inner);
    let conn_id = c.id();
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
    // `query_raw` is async: it sends the query, returns a RowStream that
    // yields rows as the underlying connection task (managed by
    // deadpool-postgres) polls the socket.
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
            finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
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
                    let v: Option<PgValue> = row.try_get(i).unwrap_or(None);
                    let val = v.map(|p| p.0).unwrap_or(Value::Null);
                    values.push(val);
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
            affected_rows: None, // not exposed by query_raw; needs simple_query for that
            warnings,
        })
        .await;

    finish(&inner, conn_id, slot_kind, conn, cursor_id_num).await;
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
