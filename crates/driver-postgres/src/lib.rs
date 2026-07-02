//! `sift-driver-postgres` — Postgres driver via `tokio-postgres` +
//! `deadpool-postgres` (ADR-003). The easy case (PHASE0.md step 7): known-
//! good driver isolates server-substrate bugs from driver bugs.
//!
//! Implements [`sift_driver_api::Driver`] and [`sift_driver_api::PgExt`].
//! SQL Server ships as a fast-follow in step 15; until that lands the
//! trait API in `driver-api` is mutable.

mod conn;
mod decode;
mod schema;
mod stream;

pub use conn::PgDriver;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, TryStreamExt};
use sift_driver_api::{
    AdvisoryKey, ConnHandle, CopyOp, CopyResult, Driver, NotificationStream, PgExt, PgSavepoint,
    ResultSetStream, TxHandle,
};
use sift_protocol::{
    Code, ConnectionSpec, CursorId, DriverError, Engine, ExecuteRequest, IsolationLevel,
    SchemaScope, SchemaSnapshot, ServerInfo, TxAccessMode as AccessMode, TxId, TxMode,
};

#[async_trait]
impl Driver for PgDriver {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        let conn = self.open_internal(spec).await?;
        let id = self.inner.conn_id.next();
        self.inner.put_free(id, conn).await;
        Ok(ConnHandle::new(id, Engine::Postgres))
    }

    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError> {
        let conn = self.take_for_op(&c).await?;
        let result = async {
            let row = conn
                .query_one("SELECT version(), current_user, current_database()", &[])
                .await
                .map_err(pg_err)?;
            Ok::<_, DriverError>(ServerInfo {
                engine: Engine::Postgres,
                server_version: row.try_get::<_, String>(0).map_err(pg_err)?,
                current_user: row.try_get::<_, String>(1).map_err(pg_err)?,
                current_database: row.try_get::<_, String>(2).map_err(pg_err)?,
            })
        }
        .await;
        self.restore_after_op(&c, conn).await;
        result
    }

    async fn schema(
        &self,
        c: ConnHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        let conn = self.take_for_op(&c).await?;
        let result = schema::introspect(&conn, &scope).await;
        self.restore_after_op(&c, conn).await;
        result
    }

    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError> {
        let conn = self.take_for_op(&c).await?;
        let sql = begin_sql(&mode);
        if let Err(e) = conn.execute(&sql, &[]).await.map_err(pg_err) {
            self.restore_after_op(&c, conn).await;
            return Err(e);
        }
        let tx_id = TxId::new(self.inner.tx_id.next());
        self.inner.put_in_tx(c.id(), tx_id.0, conn).await;
        Ok(TxHandle::new(tx_id, c, mode))
    }

    async fn commit(&self, t: TxHandle) -> Result<(), DriverError> {
        let (conn_id, conn) =
            self.inner.take_in_tx(&t.tx_id).await.ok_or_else(|| {
                DriverError::new(Code::TransactionNotFound, "transaction not open")
            })?;
        let result = conn.execute("COMMIT", &[]).await.map_err(pg_err);
        self.inner.put_free(conn_id, conn).await;
        result.map(|_| ())
    }

    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError> {
        let (conn_id, conn) =
            self.inner.take_in_tx(&t.tx_id).await.ok_or_else(|| {
                DriverError::new(Code::TransactionNotFound, "transaction not open")
            })?;
        let result = conn.execute("ROLLBACK", &[]).await.map_err(pg_err);
        self.inner.put_free(conn_id, conn).await;
        result.map(|_| ())
    }

    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        stream::execute_query(self, c, req).await
    }

    async fn cancel(&self, _c: ConnHandle, cursor: CursorId) -> Result<(), DriverError> {
        let token = {
            let entry = self
                .inner
                .cursors
                .get(&cursor.0)
                .ok_or_else(|| DriverError::new(Code::CursorNotFound, "cursor not active"))?;
            entry.1.clone()
        };
        token
            .cancel_query(tokio_postgres::NoTls)
            .await
            .map_err(pg_err)?;
        Ok(())
    }

    async fn close(&self, c: ConnHandle) -> Result<(), DriverError> {
        self.inner.remove_conn(&c).await;
        Ok(())
    }

    fn as_pg(&self) -> Option<&dyn PgExt> {
        Some(self)
    }
}

#[async_trait]
impl PgExt for PgDriver {
    async fn listen(
        &self,
        _c: ConnHandle,
        _channels: Vec<String>,
    ) -> Result<NotificationStream, DriverError> {
        // LISTEN/NOTIFY needs its own dedicated connection (stateful). The
        // dedicated `listen_pool` lands with FEATURES.md Tier 3 (collab
        // events); for Phase 0 we surface this as unsupported.
        Err(DriverError::new(
            Code::UnsupportedForEngine,
            "LISTEN/NOTIFY not yet wired (listen_pool TBD)",
        )
        .with_engine(Engine::Postgres))
    }

    async fn unlisten(&self, _c: ConnHandle, _channels: Vec<String>) -> Result<(), DriverError> {
        Err(
            DriverError::new(Code::UnsupportedForEngine, "LISTEN/NOTIFY not yet wired")
                .with_engine(Engine::Postgres),
        )
    }

    async fn copy(&self, c: ConnHandle, op: CopyOp) -> Result<CopyResult, DriverError> {
        let (conn, slot_kind) = self.inner.take_for_op(&c).await?;
        let result = async {
            match op {
                CopyOp::Export { sql } => {
                    let bytes = conn
                        .copy_out(&sql)
                        .await
                        .map_err(pg_err)?
                        .try_fold(0_u64, |total, chunk| async move {
                            Ok::<_, tokio_postgres::Error>(total + chunk.len() as u64)
                        })
                        .await
                        .map_err(pg_err)?;
                    Ok(CopyResult { bytes, rows: None })
                }
                CopyOp::Import { table, data } => {
                    let table = quote_qualified_ident(&table)?;
                    let sql = format!("COPY {table} FROM STDIN");
                    let bytes = data.len() as u64;
                    let mut stream = futures::stream::iter(vec![Ok::<_, tokio_postgres::Error>(
                        Bytes::from(data),
                    )]);
                    let mut sink = std::pin::pin!(conn.copy_in(&sql).await.map_err(pg_err)?);
                    sink.send_all(&mut stream).await.map_err(pg_err)?;
                    let rows = sink.finish().await.map_err(pg_err)?;
                    Ok(CopyResult {
                        bytes,
                        rows: Some(rows),
                    })
                }
            }
        }
        .await;
        self.inner.restore(c.id(), slot_kind, conn).await;
        result
    }

    async fn advisory_lock(&self, c: ConnHandle, key: AdvisoryKey) -> Result<(), DriverError> {
        let conn = self.take_for_op(&c).await?;
        let result = async {
            match key {
                AdvisoryKey::Int32(k1, k2) => {
                    conn.execute("SELECT pg_advisory_lock($1, $2)", &[&k1, &k2])
                        .await
                }
                AdvisoryKey::Int64(k) => conn.execute("SELECT pg_advisory_lock($1)", &[&k]).await,
            }
            .map_err(pg_err)?;
            Ok::<_, DriverError>(())
        }
        .await;
        self.restore_after_op(&c, conn).await;
        result
    }

    async fn advisory_unlock(&self, c: ConnHandle, key: AdvisoryKey) -> Result<(), DriverError> {
        let conn = self.take_for_op(&c).await?;
        let result = async {
            let unlocked = match key {
                AdvisoryKey::Int32(k1, k2) => {
                    conn.query_one("SELECT pg_advisory_unlock($1, $2)", &[&k1, &k2])
                        .await
                }
                AdvisoryKey::Int64(k) => {
                    conn.query_one("SELECT pg_advisory_unlock($1)", &[&k]).await
                }
            }
            .map_err(pg_err)?
            .try_get::<_, bool>(0)
            .map_err(pg_err)?;
            if unlocked {
                Ok(())
            } else {
                Err(DriverError::new(
                    Code::InvalidParameterValue,
                    "advisory lock was not held by this connection",
                )
                .with_engine(Engine::Postgres))
            }
        }
        .await;
        self.restore_after_op(&c, conn).await;
        result
    }

    async fn savepoint(&self, t: &TxHandle, name: &str) -> Result<PgSavepoint, DriverError> {
        validate_ident(name)?;
        let (conn_id, conn) =
            self.inner.take_in_tx(&t.tx_id).await.ok_or_else(|| {
                DriverError::new(Code::TransactionNotFound, "transaction not open")
            })?;
        let sql = format!("SAVEPOINT {name}");
        let result = conn.execute(&sql, &[]).await.map_err(pg_err);
        self.inner.put_in_tx(conn_id, t.tx_id.0, conn).await;
        result.map(|_| ())?;
        Ok(PgSavepoint {
            tx: t.tx_id,
            name: name.to_string(),
        })
    }

    async fn rollback_to(&self, sp: PgSavepoint) -> Result<(), DriverError> {
        validate_ident(&sp.name)?;
        let (conn_id, conn) =
            self.inner.take_in_tx(&sp.tx).await.ok_or_else(|| {
                DriverError::new(Code::TransactionNotFound, "transaction not open")
            })?;
        let sql = format!("ROLLBACK TO SAVEPOINT {}", sp.name);
        let result = conn.execute(&sql, &[]).await.map_err(pg_err);
        self.inner.put_in_tx(conn_id, sp.tx.0, conn).await;
        result.map(|_| ())
    }

    async fn release_savepoint(&self, sp: PgSavepoint) -> Result<(), DriverError> {
        validate_ident(&sp.name)?;
        let (conn_id, conn) =
            self.inner.take_in_tx(&sp.tx).await.ok_or_else(|| {
                DriverError::new(Code::TransactionNotFound, "transaction not open")
            })?;
        let sql = format!("RELEASE SAVEPOINT {}", sp.name);
        let result = conn.execute(&sql, &[]).await.map_err(pg_err);
        self.inner.put_in_tx(conn_id, sp.tx.0, conn).await;
        result.map(|_| ())
    }
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/// Translate tokio-postgres errors into our driver-agnostic [`DriverError`].
///
/// SQLSTATE (5-char code, when present) maps onto our stable [`Code`]. The
/// raw driver error text is preserved in `message` — the protocol layer is
/// responsible for not leaking it across the wire if that's a concern.
pub(crate) fn pg_err(e: tokio_postgres::Error) -> DriverError {
    let sqlstate = e.code().map(|c| c.code().to_string());
    let code = match sqlstate.as_deref() {
        // Connection class 08*
        Some(s) if s.starts_with("08") => Code::ConnectionFailed,
        // Auth class 28*
        Some(s) if s.starts_with("28") => Code::AuthFailed,
        // Query canceled
        Some("57014") => Code::QueryCanceled,
        // Syntax
        Some("42601") => Code::SyntaxError,
        // Undefined object (42P01 = undefined_table, 42704 = undefined_object,
        // 42883 = undefined_function, 42P02 = undefined_parameter)
        Some("42P01" | "42704" | "42883" | "42P02") => Code::UndefinedObject,
        // Duplicate object
        Some("42P04" | "42710" | "42701" | "42723") => Code::DuplicateObject,
        // Data exception class 22* (cover the whole class)
        Some(s) if s.starts_with("22") => Code::InvalidParameterValue,
        // Internal / fatal
        Some(s) if s.starts_with("57") || s.starts_with("58") || s.starts_with("XX") => {
            Code::DriverInternal
        }
        _ => Code::DriverInternal,
    };
    let mut err = DriverError::new(code, e.to_string()).with_engine(Engine::Postgres);
    if let Some(s) = sqlstate {
        err = err.with_sqlstate(s);
    }
    err
}

fn begin_sql(mode: &TxMode) -> String {
    let iso = match mode.isolation {
        IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
        IsolationLevel::ReadCommitted => "READ COMMITTED",
        IsolationLevel::RepeatableRead => "REPEATABLE READ",
        // PG has no native SNAPSHOT; PG's SERIALIZABLE is the closest. Caller
        // picks one of the others; Snapshot maps to RepeatableRead for safety.
        IsolationLevel::Snapshot => "REPEATABLE READ",
        IsolationLevel::Serializable => "SERIALIZABLE",
    };
    let access = match mode.access {
        AccessMode::ReadWrite => "",
        AccessMode::ReadOnly => " READ ONLY",
    };
    format!("BEGIN ISOLATION LEVEL {iso}{access}")
}

/// PG identifiers are [A-Za-z_][A-Za-z0-9_]*. Reject anything else to avoid
/// SQL injection through engine-specific ops (savepoint names, advisory
/// lock keys, channel names).
pub(crate) fn validate_ident(name: &str) -> Result<(), DriverError> {
    let valid = name
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(DriverError::new(
            Code::InvalidParameterValue,
            "identifier must be [A-Za-z_][A-Za-z0-9_]*",
        ));
    }
    Ok(())
}

fn quote_qualified_ident(name: &str) -> Result<String, DriverError> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.is_empty() || parts.len() > 2 {
        return Err(DriverError::new(
            Code::InvalidParameterValue,
            "table name must be `table` or `schema.table`",
        ));
    }
    let mut quoted = Vec::with_capacity(parts.len());
    for part in parts {
        validate_ident(part)?;
        quoted.push(format!("\"{part}\""));
    }
    Ok(quoted.join("."))
}

#[cfg(test)]
mod copy_tests {
    use super::*;

    #[test]
    fn quote_qualified_ident_accepts_table_and_schema() {
        assert_eq!(quote_qualified_ident("users").unwrap(), "\"users\"");
        assert_eq!(
            quote_qualified_ident("public.users").unwrap(),
            "\"public\".\"users\""
        );
    }

    #[test]
    fn quote_qualified_ident_rejects_injection() {
        assert!(quote_qualified_ident("public.users;drop").is_err());
        assert!(quote_qualified_ident("a.b.c").is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_ident_accepts_legal_names() {
        assert!(validate_ident("sp1").is_ok());
        assert!(validate_ident("_private").is_ok());
        assert!(validate_ident("Save_Point_42").is_ok());
    }

    #[test]
    fn validate_ident_rejects_injection_attempts() {
        assert!(validate_ident("").is_err());
        assert!(validate_ident("1abc").is_err()); // starts with digit
        assert!(validate_ident("name; COMMIT").is_err());
        assert!(validate_ident("a'b").is_err());
        assert!(validate_ident("a--b").is_err());
        assert!(validate_ident("a/*b*/").is_err());
    }

    #[test]
    fn begin_sql_reflects_isolation_and_access() {
        let m = TxMode {
            isolation: IsolationLevel::Serializable,
            access: AccessMode::ReadOnly,
        };
        assert_eq!(
            begin_sql(&m),
            "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY"
        );
    }

    #[test]
    fn begin_sql_defaults_to_read_write() {
        let m = TxMode {
            isolation: IsolationLevel::ReadCommitted,
            access: AccessMode::ReadWrite,
        };
        assert_eq!(begin_sql(&m), "BEGIN ISOLATION LEVEL READ COMMITTED");
    }

    #[test]
    fn begin_sql_maps_snapshot_to_repeatable_read() {
        // PG has no native SNAPSHOT isolation in `BEGIN ISOLATION LEVEL`;
        // SERIALIZABLE is the strict superset. We pick REPEATABLE READ for
        // safety until a SNAPSHOT-via-prepared-statement path lands.
        let m = TxMode {
            isolation: IsolationLevel::Snapshot,
            access: AccessMode::ReadWrite,
        };
        assert_eq!(begin_sql(&m), "BEGIN ISOLATION LEVEL REPEATABLE READ");
    }
}
