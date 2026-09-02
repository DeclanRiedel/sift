//! `sift-driver-postgres` — Postgres driver via `tokio-postgres` +
//! `deadpool-postgres` (ADR-003). Wraps a known-good driver so server-
//! substrate bugs stay isolated from driver bugs.
//!
//! Implements [`sift_driver_api::Driver`] and [`sift_driver_api::PgExt`].

mod conn;
mod decode;
mod progress;
mod schema;
mod stream;

pub use conn::PgDriver;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{future::poll_fn, SinkExt, TryStreamExt};
use sift_driver_api::{
    AdvisoryKey, ConnHandle, CopyOp, CopyResult, Driver, NotificationStream, PgExt, PgNotification,
    PgSavepoint, ResultSetStream, TxHandle,
};
use sift_protocol::{
    Code, ConnectionSpec, CursorId, DriverError, Engine, ExecuteRequest, IsolationLevel,
    SchemaScope, SchemaSnapshot, ServerInfo, TxAccessMode as AccessMode, TxId, TxMode,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_postgres::{AsyncMessage, Client, Connection};

#[async_trait]
impl Driver for PgDriver {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", host = %spec.host))]
    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        let conn = self
            .open_internal(spec)
            .await
            .map_err(|error| contextualize_open_error(spec, error))?;
        let backend_pid = conn
            .query_one("SELECT pg_catalog.pg_backend_pid()", &[])
            .await
            .ok()
            .and_then(|row| row.try_get::<_, i32>(0).ok())
            .unwrap_or(0);
        let id = self.inner.conn_id.next();
        self.inner.put_free(id, conn).await;
        self.inner.put_spec(id, spec.clone());
        self.inner.backend_pids.insert(id, backend_pid);
        // Pre-warm additional pool slots when the spec requests it.
        // Prewarming is background work by definition, so spawn it rather
        // than blocking `open` on `pool_min_size - 1` concurrent TCP+TLS+PG
        // handshakes: `open` already has one working conn and the caller
        // can proceed immediately. Best-effort — a prewarm failure is
        // logged inside `prewarm_pool`, not surfaced.
        if let Some(sift_protocol::EngineConnectionSpec::Postgres(p)) = &spec.engine_specific {
            if let Some(min) = p.pool_min_size {
                let extra = (min as usize).saturating_sub(1);
                if extra > 0 {
                    let driver = self.clone();
                    let spec = spec.clone();
                    tokio::spawn(async move {
                        driver.prewarm_pool(&spec, extra).await;
                    });
                }
            }
        }
        Ok(ConnHandle::new(id, Engine::Postgres))
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id()))]
    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError> {
        let conn = self.take_for_op(&c).await?;
        let warm_slots = self.inner.pool_warm_slots_for(c.id());
        let result = async {
            let row = conn
                .query_one("SELECT version(), current_user, current_database()", &[])
                .await
                .map_err(pg_err)?;
            Ok::<_, DriverError>(ServerInfo {
                provider: Engine::Postgres.provider_ref(env!("CARGO_PKG_VERSION")),
                server_version: row.try_get::<_, String>(0).map_err(pg_err)?,
                current_user: row.try_get::<_, String>(1).map_err(pg_err)?,
                current_database: row.try_get::<_, String>(2).map_err(pg_err)?,
                pool_warm_slots: warm_slots,
            })
        }
        .await;
        self.restore_after_op(&c, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id(), depth = ?scope.depth))]
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

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id()))]
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

    #[tracing::instrument(skip_all, fields(engine = "postgres", tx = t.tx_id.0))]
    async fn commit(&self, t: TxHandle) -> Result<(), DriverError> {
        let (conn_id, conn) = self
            .inner
            .take_in_tx(t.conn.id(), &t.tx_id)
            .await
            .ok_or_else(|| DriverError::new(Code::TransactionNotFound, "transaction not open"))?;
        let result = conn.execute("COMMIT", &[]).await.map_err(pg_err);
        self.inner.put_free(conn_id, conn).await;
        result.map(|_| ())
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", tx = t.tx_id.0))]
    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError> {
        let (conn_id, conn) = self
            .inner
            .take_in_tx(t.conn.id(), &t.tx_id)
            .await
            .ok_or_else(|| DriverError::new(Code::TransactionNotFound, "transaction not open"))?;
        let result = conn.execute("ROLLBACK", &[]).await.map_err(pg_err);
        self.inner.put_free(conn_id, conn).await;
        result.map(|_| ())
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id()))]
    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        stream::execute_query(self, c, req).await
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", cursor = cursor.0))]
    async fn cancel(&self, c: ConnHandle, cursor: CursorId) -> Result<(), DriverError> {
        let token = {
            let entry = self
                .inner
                .cursors
                .get(&cursor.0)
                .ok_or_else(|| DriverError::new(Code::CursorNotFound, "cursor not active"))?;
            // Ownership check: reject cancels for a cursor that does not
            // belong to this ConnHandle. Cursor ids are monotonic across
            // all conns, so without this an authenticated caller with any
            // ConnHandle could cancel another user's query by guessing.
            if entry.conn_id != c.id() {
                return Err(DriverError::new(
                    Code::CursorNotFound,
                    "cursor does not belong to this connection",
                )
                .with_engine(Engine::Postgres));
            }
            entry.cancel_token.clone()
        };
        // Match the SSL mode the original conn used. Postgres deployments
        // configured with `hostssl` reject a NoTls cancel socket, and the
        // caller would then observe "cancel succeeded" while the query
        // kept running server-side.
        let ssl_mode = self
            .inner
            .spec_for(c.id())
            .and_then(|s| s.ssl_mode)
            .unwrap_or(sift_protocol::SslMode::Prefer);
        let cancel = async {
            match ssl_mode {
                sift_protocol::SslMode::Require
                | sift_protocol::SslMode::VerifyCa
                | sift_protocol::SslMode::VerifyFull => {
                    let tls = conn::native_tls_connector()?;
                    token.cancel_query(tls).await.map_err(pg_err)?;
                }
                _ => {
                    token
                        .cancel_query(tokio_postgres::NoTls)
                        .await
                        .map_err(pg_err)?;
                }
            }
            Ok::<_, DriverError>(())
        };
        tokio::time::timeout(Duration::from_secs(5), cancel)
            .await
            .map_err(|_| {
                DriverError::new(Code::QueryTimedOut, "Postgres cancel timed out")
                    .with_engine(Engine::Postgres)
            })??;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id()))]
    async fn close(&self, c: ConnHandle) -> Result<(), DriverError> {
        self.inner.remove_conn(&c).await;
        self.inner.backend_pids.remove(&c.id());
        Ok(())
    }

    fn as_pg(&self) -> Option<&dyn PgExt> {
        Some(self)
    }
}

#[async_trait]
impl PgExt for PgDriver {
    async fn observe_progress(
        &self,
        c: ConnHandle,
        cursor: CursorId,
    ) -> Result<Option<sift_driver_api::NativeProgressStream>, DriverError> {
        progress::observe(self, &c, cursor)
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id(), channel_count = channels.len()))]
    async fn listen(
        &self,
        c: ConnHandle,
        channels: Vec<String>,
    ) -> Result<NotificationStream, DriverError> {
        if channels.is_empty() {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "at least one LISTEN channel is required",
            )
            .with_engine(Engine::Postgres));
        }
        let spec = self.inner.spec_for(c.id()).ok_or_else(|| {
            DriverError::new(Code::ConnectionFailed, "no connection spec for handle")
                .with_engine(Engine::Postgres)
        })?;
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let cfg = pg_connect_config(&spec)?;
        let ssl_mode = spec.ssl_mode.unwrap_or(sift_protocol::SslMode::Prefer);
        let channels_set: std::collections::HashSet<String> = channels.iter().cloned().collect();
        let client = if matches!(
            ssl_mode,
            sift_protocol::SslMode::VerifyCa | sift_protocol::SslMode::VerifyFull
        ) {
            let tls = conn::native_tls_connector()?;
            let (client, connection) = cfg.connect(tls).await.map_err(pg_err)?;
            let client = Arc::new(client);
            spawn_notification_pump(Arc::clone(&client), connection, tx);
            listen_channels(&client, channels).await?;
            client
        } else {
            let (client, connection) = cfg.connect(tokio_postgres::NoTls).await.map_err(pg_err)?;
            let client = Arc::new(client);
            spawn_notification_pump(Arc::clone(&client), connection, tx);
            listen_channels(&client, channels).await?;
            client
        };
        // Track the dedicated LISTEN client and the channels it
        // subscribed to so `unlisten` (and `close`) can reach only the
        // clients that actually care about a given channel.
        self.inner
            .listens
            .entry(c.id())
            .or_default()
            .push(conn::ListenEntry {
                client,
                channels: channels_set,
            });
        Ok(NotificationStream { notifications: rx })
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id(), channel_count = channels.len()))]
    async fn unlisten(&self, c: ConnHandle, channels: Vec<String>) -> Result<(), DriverError> {
        for channel in &channels {
            validate_ident(channel)?;
        }
        // Snapshot the (client, its channels) tuples out of the DashMap
        // so we don't hold a shard lock across the .await. Also update
        // each entry's channel set in place before releasing.
        let (targets, remove_conn_listens): (
            Vec<(Arc<tokio_postgres::Client>, Vec<String>)>,
            bool,
        ) = {
            let Some(mut entry) = self.inner.listens.get_mut(&c.id()) else {
                return Ok(());
            };
            let mut out = Vec::new();
            for listen in entry.value_mut().iter_mut() {
                let hits: Vec<String> = channels
                    .iter()
                    .filter(|ch| listen.channels.remove(*ch))
                    .cloned()
                    .collect();
                if !hits.is_empty() {
                    out.push((Arc::clone(&listen.client), hits));
                }
            }
            entry
                .value_mut()
                .retain(|listen| !listen.channels.is_empty());
            let empty = entry.value().is_empty();
            (out, empty)
        };
        if remove_conn_listens {
            self.inner.listens.remove(&c.id());
        }
        for (client, chans) in &targets {
            for channel in chans {
                client
                    .batch_execute(&format!("UNLISTEN {}", quote_ident(channel)?))
                    .await
                    .map_err(pg_err)?;
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id()))]
    async fn copy(&self, c: ConnHandle, op: CopyOp) -> Result<CopyResult, DriverError> {
        let (conn, slot_kind) = self.inner.take_for_op(&c).await?;
        let result = async {
            match op {
                CopyOp::Export { sql } => {
                    let data = conn
                        .copy_out(&sql)
                        .await
                        .map_err(pg_err)?
                        .try_fold(Vec::new(), |mut out, chunk| async move {
                            out.extend_from_slice(&chunk);
                            Ok::<_, tokio_postgres::Error>(out)
                        })
                        .await
                        .map_err(pg_err)?;
                    Ok(CopyResult {
                        bytes: data.len() as u64,
                        rows: None,
                        data,
                    })
                }
                CopyOp::Import {
                    table,
                    columns,
                    data,
                    delimiter,
                    header,
                    null_value,
                } => {
                    let table = quote_qualified_ident(&table)?;
                    if !delimiter.is_ascii() || delimiter == b'\'' || delimiter == b'\\' {
                        return Err(DriverError::new(
                            Code::InvalidParameterValue,
                            "COPY delimiter must be ASCII and cannot be a quote or backslash",
                        )
                        .with_engine(Engine::Postgres));
                    }
                    let columns = columns
                        .iter()
                        .map(|column| quote_ident(column))
                        .collect::<Result<Vec<_>, _>>()?;
                    if columns.is_empty() {
                        return Err(DriverError::new(
                            Code::InvalidParameterValue,
                            "COPY import requires at least one column",
                        )
                        .with_engine(Engine::Postgres));
                    }
                    let null_clause = null_value
                        .map(|value| format!(", NULL '{}'", value.replace('\'', "''")))
                        .unwrap_or_default();
                    let sql = format!(
                        "COPY {table} ({}) FROM STDIN WITH (FORMAT csv, HEADER {}, DELIMITER '{}'{null_clause})",
                        columns.join(", "),
                        if header { "true" } else { "false" },
                        delimiter as char,
                    );
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
                        data: Vec::new(),
                    })
                }
            }
        }
        .await;
        self.inner.restore(c.id(), slot_kind, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id(), key = ?key))]
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

    #[tracing::instrument(skip_all, fields(engine = "postgres", conn = c.id(), key = ?key))]
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

    #[tracing::instrument(skip_all, fields(engine = "postgres", tx = t.tx_id.0, name = %name))]
    async fn savepoint(&self, t: &TxHandle, name: &str) -> Result<PgSavepoint, DriverError> {
        validate_ident(name)?;
        let (conn_id, conn) = self
            .inner
            .take_in_tx(t.conn.id(), &t.tx_id)
            .await
            .ok_or_else(|| DriverError::new(Code::TransactionNotFound, "transaction not open"))?;
        let sql = format!("SAVEPOINT {name}");
        let result = conn.execute(&sql, &[]).await.map_err(pg_err);
        self.inner.put_in_tx(conn_id, t.tx_id.0, conn).await;
        result.map(|_| ())?;
        Ok(PgSavepoint {
            tx: t.tx_id,
            conn: t.conn.clone(),
            name: name.to_string(),
        })
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", tx = sp.tx.0, name = %sp.name))]
    async fn rollback_to(&self, sp: PgSavepoint) -> Result<(), DriverError> {
        validate_ident(&sp.name)?;
        let (conn_id, conn) = self
            .inner
            .take_in_tx(sp.conn.id(), &sp.tx)
            .await
            .ok_or_else(|| DriverError::new(Code::TransactionNotFound, "transaction not open"))?;
        let sql = format!("ROLLBACK TO SAVEPOINT {}", sp.name);
        let result = conn.execute(&sql, &[]).await.map_err(pg_err);
        self.inner.put_in_tx(conn_id, sp.tx.0, conn).await;
        result.map(|_| ())
    }

    #[tracing::instrument(skip_all, fields(engine = "postgres", tx = sp.tx.0, name = %sp.name))]
    async fn release_savepoint(&self, sp: PgSavepoint) -> Result<(), DriverError> {
        validate_ident(&sp.name)?;
        let (conn_id, conn) = self
            .inner
            .take_in_tx(sp.conn.id(), &sp.tx)
            .await
            .ok_or_else(|| DriverError::new(Code::TransactionNotFound, "transaction not open"))?;
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
/// Structured database errors are projected into an actionable message. The
/// generic `Display` implementation intentionally reduces these to `db error`,
/// which is not useful to someone correcting a query.
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
    let message = e
        .as_db_error()
        .map(postgres_db_error_message)
        .unwrap_or_else(|| e.to_string());
    let mut err = DriverError::new(code, message).with_engine(Engine::Postgres);
    if let Some(s) = sqlstate {
        err = err.with_sqlstate(s);
    }
    err
}

fn postgres_db_error_message(error: &tokio_postgres::error::DbError) -> String {
    let position = error.position().map(|position| match position {
        tokio_postgres::error::ErrorPosition::Original(position) => *position,
        tokio_postgres::error::ErrorPosition::Internal { position, .. } => *position,
    });
    format_postgres_db_error(error.message(), error.detail(), error.hint(), position)
}

fn format_postgres_db_error(
    message: &str,
    detail: Option<&str>,
    hint: Option<&str>,
    position: Option<u32>,
) -> String {
    let mut parts = vec![message.to_string()];
    if let Some(detail) = detail {
        parts.push(format!("Detail: {detail}"));
    }
    if let Some(hint) = hint {
        parts.push(format!("Hint: {hint}"));
    }
    if let Some(position) = position {
        parts.push(format!("Position: {position}"));
    }
    parts.join("\n")
}

/// Replace opaque connect/authentication errors with safe, actionable context.
///
/// In particular, tokio-postgres deliberately renders some database errors as
/// `db error`. The SQLSTATE still tells us what happened, so provide useful
/// guidance without ever copying the password or raw server error into the
/// user-visible message.
fn contextualize_open_error(spec: &ConnectionSpec, mut error: DriverError) -> DriverError {
    let target = connection_target(spec);
    let loopback_hint = is_loopback_host(&spec.host).then_some(
        " localhost refers to the machine running the Sift server; use the database server hostname or IP when PostgreSQL runs elsewhere.",
    );

    match &error.code {
        Code::AuthFailed => {
            error.message = format!(
                "PostgreSQL rejected the credentials at {target}. Verify the username, password, database, and target server.{}",
                loopback_hint.unwrap_or_default()
            );
        }
        Code::ConnectionFailed => {
            error.message = format!(
                "Could not connect to PostgreSQL at {target}. Verify the hostname, port, and network access.{}",
                loopback_hint.unwrap_or_default()
            );
        }
        _ => {}
    }

    error
}

fn connection_target(spec: &ConnectionSpec) -> String {
    let host: String = spec
        .host
        .chars()
        .take(255)
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect();

    if spec.host.starts_with('/') {
        format!("Unix socket {host}")
    } else {
        format!("{host}:{}", spec.port.unwrap_or(5432))
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1" | "[::1]")
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

fn pg_connect_config(spec: &ConnectionSpec) -> Result<tokio_postgres::Config, DriverError> {
    let mut cfg = tokio_postgres::Config::new();
    if spec.host.starts_with('/') {
        cfg.host_path(&spec.host);
    } else {
        cfg.host(&spec.host);
    }
    if let Some(port) = spec.port {
        cfg.port(port);
    }
    if let Some(database) = &spec.database {
        cfg.dbname(database);
    }
    cfg.user(&spec.user);
    if let Some(password) = &spec.password {
        cfg.password(password);
    }
    cfg.application_name("sift-listen");
    cfg.ssl_mode(
        match spec.ssl_mode.unwrap_or(sift_protocol::SslMode::Prefer) {
            sift_protocol::SslMode::Disable => tokio_postgres::config::SslMode::Disable,
            sift_protocol::SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            sift_protocol::SslMode::Require
            | sift_protocol::SslMode::VerifyCa
            | sift_protocol::SslMode::VerifyFull => tokio_postgres::config::SslMode::Require,
        },
    );

    if let Some(sift_protocol::EngineConnectionSpec::Postgres(pg)) = &spec.engine_specific {
        if let Some(search_path) = &pg.search_path {
            cfg.options(format_search_path_option(search_path)?);
        }
        if let Some(timeout) = pg.connect_timeout_secs {
            cfg.connect_timeout(std::time::Duration::from_secs(timeout as u64));
        }
    }
    Ok(cfg)
}

async fn listen_channels(client: &Client, channels: Vec<String>) -> Result<(), DriverError> {
    for channel in channels {
        client
            .batch_execute(&format!("LISTEN {}", quote_ident(&channel)?))
            .await
            .map_err(pg_err)?;
    }
    Ok(())
}

fn spawn_notification_pump<S, T>(
    client: Arc<Client>,
    mut connection: Connection<S, T>,
    notifications: tokio::sync::mpsc::Sender<PgNotification>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let _client = client;
        loop {
            let message = tokio::select! {
                biased;
                _ = notifications.closed() => break,
                message = poll_fn(|cx| connection.poll_message(cx)) => message,
            };
            match message {
                Some(Ok(AsyncMessage::Notification(notification))) => {
                    let notification = PgNotification {
                        channel: notification.channel().to_string(),
                        payload: notification.payload().to_string(),
                    };
                    if notifications.send(notification).await.is_err() {
                        break;
                    }
                }
                Some(Ok(AsyncMessage::Notice(notice))) => {
                    tracing::debug!(message = %notice.message(), "postgres listen notice");
                    tokio::task::yield_now().await;
                }
                Some(Ok(_)) => {
                    tokio::task::yield_now().await;
                }
                Some(Err(error)) => {
                    tracing::warn!(error = %error, "postgres listen connection ended");
                    break;
                }
                None => break,
            }
        }
    });
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

fn quote_ident(name: &str) -> Result<String, DriverError> {
    validate_ident(name)?;
    Ok(format!("\"{name}\""))
}

pub(crate) fn format_search_path_option(search_path: &[String]) -> Result<String, DriverError> {
    if search_path.is_empty() {
        return Err(DriverError::new(
            Code::InvalidParameterValue,
            "search_path must contain at least one schema",
        ));
    }
    let mut entries = Vec::with_capacity(search_path.len());
    for entry in search_path {
        if entry == "$user" {
            entries.push("\"$user\"".to_string());
        } else {
            validate_ident(entry)?;
            entries.push(entry.clone());
        }
    }
    Ok(format!("-c search_path={}", entries.join(",")))
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
        quoted.push(quote_ident(part)?);
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

    fn connection_spec(host: &str) -> ConnectionSpec {
        ConnectionSpec {
            host: host.to_string(),
            port: Some(5433),
            database: Some("inventory".to_string()),
            user: "app".to_string(),
            password: Some("do-not-leak".to_string()),
            ssl_mode: None,
            engine_specific: None,
        }
    }

    #[test]
    fn auth_failure_explains_localhost_without_leaking_credentials() {
        let original = DriverError::new(Code::AuthFailed, "db error")
            .with_engine(Engine::Postgres)
            .with_sqlstate("28P01");

        let error = contextualize_open_error(&connection_spec("localhost"), original);

        assert_eq!(error.code, Code::AuthFailed);
        assert_eq!(error.native_code.as_deref(), Some("28P01"));
        assert!(error.message.contains("localhost:5433"));
        assert!(error.message.contains("machine running the Sift server"));
        assert!(error.message.contains("database server hostname or IP"));
        assert!(!error.message.contains("do-not-leak"));
        assert!(!error.message.contains("db error"));
    }

    #[test]
    fn remote_auth_failure_names_target_without_localhost_hint() {
        let original = DriverError::new(Code::AuthFailed, "db error");

        let error = contextualize_open_error(&connection_spec("db.internal"), original);

        assert!(error.message.contains("db.internal:5433"));
        assert!(!error.message.contains("machine running the Sift server"));
        assert!(!error.message.contains("do-not-leak"));
    }

    #[test]
    fn connection_failure_explains_localhost_without_leaking_credentials() {
        let original = DriverError::new(Code::ConnectionFailed, "connection refused");

        let error = contextualize_open_error(&connection_spec("127.0.0.1"), original);

        assert!(error.message.contains("127.0.0.1:5433"));
        assert!(error.message.contains("machine running the Sift server"));
        assert!(!error.message.contains("do-not-leak"));
        assert!(!error.message.contains("connection refused"));
    }

    #[test]
    fn database_error_message_keeps_actionable_server_context() {
        assert_eq!(
            format_postgres_db_error(
                "relation \"missing_table\" does not exist",
                Some("The table was removed by a migration."),
                Some("Check the active schema."),
                Some(15),
            ),
            "relation \"missing_table\" does not exist\n\
             Detail: The table was removed by a migration.\n\
             Hint: Check the active schema.\n\
             Position: 15"
        );
    }

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
    fn format_search_path_option_accepts_safe_entries() {
        let option = format_search_path_option(&["$user".into(), "public".into()]).unwrap();
        assert_eq!(option, r#"-c search_path="$user",public"#);
    }

    #[test]
    fn format_search_path_option_rejects_startup_option_injection() {
        for entry in [
            "has space",
            "public,evil",
            "x -c statement_timeout=0",
            "\"quoted\"",
        ] {
            assert!(
                format_search_path_option(&[entry.to_string()]).is_err(),
                "{entry:?} should be rejected"
            );
        }
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
