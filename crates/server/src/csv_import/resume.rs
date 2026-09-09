//! Target-side checkpoints share the transaction that imports each chunk.
use super::*;
use sha2::{Digest, Sha256};
use sift_protocol::{
    BeginTransactionRequest, EndTransactionRequest, IsolationLevel, OperationKind, TxHandleRef,
    TxMode,
};

pub async fn import_with_checkpoint(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: CsvImportRequest,
    checkpoint_table: &str,
    run_id: &str,
    authority: &str,
) -> ApiResult<CsvImportResponse> {
    if request.create_table
        || request.conflict_policy != CsvConflictPolicy::Abort
        || request.resume_from_row != 0
        || !request.type_mappings.is_empty()
        || uuid::Uuid::parse_str(run_id).is_err()
        || checkpoint_table.eq_ignore_ascii_case(&request.table)
    {
        return Err(ApiError::BadRequest("durable resume requires an existing target, a distinct checkpoint table, a UUID run_id, abort policy and no manual offset or type overrides".into()));
    }
    let target_path = table_path(&request.table)?;
    let checkpoint_path = table_path(checkpoint_table)?;
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ImportCsv,
        None,
        &[&target_path, &checkpoint_path],
    )?;
    let engine = store.conn_entry(session, connection)?.driver.engine();
    let prepared = prepare(&request)?;
    let target = qualified_table(&request.table, engine)?;
    let checkpoint = qualified_table(checkpoint_table, engine)?;
    let target_types =
        target_column_types(store, session, connection, &request.table, &prepared).await?;
    let total = prepared.records.len() as u64;
    if request.dry_run {
        return Ok(CsvImportResponse {
            table: request.table,
            columns: prepared.columns,
            table_created: false,
            rows_inserted: 0,
            rows_skipped: 0,
            rows_validated: total,
            resume_from_row: 0,
            dry_run: true,
            quarantined_rows: vec![],
        });
    }
    let mut hash = Sha256::new();
    hash.update(authority.as_bytes());
    hash.update([0]);
    hash.update(Sha256::digest(&request.data));
    hash.update(
        serde_json::to_vec(&(
            &request.table,
            request.header,
            request.delimiter,
            &request.null_value,
        ))
        .map_err(|e| ApiError::Internal(e.to_string()))?,
    );
    hash.update(serde_json::to_vec(&target_types).map_err(|e| ApiError::Internal(e.to_string()))?);
    let digest = format!("{:x}", hash.finalize());
    let ddl = format!("CREATE TABLE {checkpoint} (run_id varchar(36) PRIMARY KEY, digest varchar(64) NOT NULL, next_row bigint NOT NULL)");
    let ddl = match engine {
        Engine::SqlServer => format!(
            "IF OBJECT_ID(N'{}', N'U') IS NULL {ddl}",
            checkpoint_table.replace('\'', "''")
        ),
        _ => ddl.replacen("CREATE TABLE", "CREATE TABLE IF NOT EXISTS", 1),
    };
    store
        .execute_http_as(
            session,
            execute_request(connection, ddl, vec![]),
            OperationKind::ImportCsv,
        )
        .await?;
    let mut imported = 0;
    loop {
        let transaction = store
            .begin_transaction_as(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode {
                        isolation: IsolationLevel::Serializable,
                        ..Default::default()
                    },
                },
                OperationKind::ImportCsv,
            )
            .await?;
        let tx = TxHandleRef {
            tx_id: transaction.tx_id,
            connection,
            mode: transaction.mode,
        };
        let work = async {
            let p1 = parameter(engine, 1);
            let p2 = parameter(engine, 2);
            let initialize = format!("INSERT INTO {checkpoint} (run_id, digest, next_row) VALUES ({p1}, {p2}, 0)");
            let initialize = match engine {
                Engine::SqlServer => format!("IF NOT EXISTS (SELECT 1 FROM {checkpoint} WITH (UPDLOCK, HOLDLOCK) WHERE run_id = {p1}) {initialize}"),
                _ => format!("{initialize} ON CONFLICT (run_id) DO NOTHING"),
            };
            execute(store, session, &tx, initialize, vec![Value::Text(run_id.into()), Value::Text(digest.clone())]).await?;
            let locked_table = if engine == Engine::SqlServer { format!("{checkpoint} WITH (UPDLOCK, HOLDLOCK)") } else { checkpoint.clone() };
            let lock = if engine == Engine::Postgres { " FOR UPDATE" } else { "" };
            let state = execute(store, session, &tx, format!("SELECT digest, next_row FROM {locked_table} WHERE run_id = {p1}{lock}"), vec![Value::Text(run_id.into())]).await?;
            let row = state.rows.first().ok_or_else(|| ApiError::Conflict("checkpoint row missing".into()))?;
            if row.values.first() != Some(&Value::Text(digest.clone())) {
                return Err(ApiError::Conflict("resume source, target, recipe or owner changed".into()));
            }
            let offset = row.values.get(1).and_then(value_i64).and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n <= total).ok_or_else(|| ApiError::Conflict("invalid checkpoint offset".into()))?;
            let end = total.min(offset + 100);
            let columns = prepared.columns.iter().map(|c| quote_ident(&c.name, engine)).collect::<Vec<_>>().join(", ");
            for record in &prepared.records[offset as usize..end as usize] {
                let mut params = Vec::new();
                let values = record.iter().zip(&target_types).map(|(value, target_type)| match value {
                    None => "NULL".to_owned(),
                    Some(value) => {
                        params.push(Value::Text(value.clone()));
                        cast_placeholder(engine, params.len(), target_type)
                    }
                }).collect::<Vec<_>>().join(", ");
                execute(store, session, &tx, format!("INSERT INTO {target} ({columns}) VALUES ({values})"), params).await?;
            }
            execute(store, session, &tx, format!("UPDATE {checkpoint} SET next_row = {end} WHERE run_id = {p1}"), vec![Value::Text(run_id.into())]).await?;
            Ok::<_, ApiError>((end, end - offset))
        }.await;
        let end_request = EndTransactionRequest {
            connection,
            tx_id: transaction.tx_id,
        };
        let work = match work {
            Ok(progress) => store
                .commit_transaction_as(session, end_request.clone(), OperationKind::ImportCsv)
                .await
                .map(|()| progress),
            Err(error) => Err(error),
        };
        match work {
            Ok((offset, added)) => {
                imported += added;
                if offset == total {
                    break;
                }
            }
            Err(error) => {
                if store
                    .rollback_transaction_as(session, end_request, OperationKind::ImportCsv)
                    .await
                    .is_err()
                {
                    let _ = store.close_connection(session, connection).await;
                }
                return Err(error);
            }
        }
    }
    Ok(CsvImportResponse {
        table: request.table,
        columns: prepared.columns,
        table_created: false,
        rows_inserted: imported,
        rows_skipped: 0,
        rows_validated: total,
        resume_from_row: total,
        dry_run: false,
        quarantined_rows: vec![],
    })
}

fn parameter(engine: Engine, index: usize) -> String {
    match engine {
        Engine::Postgres => format!("${index}"),
        Engine::SqlServer => format!("@P{index}"),
        Engine::Sqlite => format!("?{index}"),
    }
}

async fn execute(
    store: &SessionStore,
    session: SessionId,
    tx: &TxHandleRef,
    sql: String,
    params: Vec<Value>,
) -> ApiResult<sift_protocol::ExecuteResponse> {
    let mut request = execute_request(tx.connection, sql, params);
    request.tx = Some(tx.clone());
    store
        .execute_http_as(session, request, OperationKind::ImportCsv)
        .await
}
