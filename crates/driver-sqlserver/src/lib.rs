//! `sift-driver-sqlserver` — SQL Server via tiberius (ADR-003).
//!
//! Phase 0 hard-case implementation. This is intentionally conservative:
//! one in-flight operation per connection, streamed result pages, native
//! metadata preserved through the protocol escape hatches.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use sift_driver_api::{ConnHandle, Driver, IdCounter, MssqlExt, ResultSetStream, TxHandle};
use sift_protocol::{
    Code, ColumnMetadata, ConnectionSpec, CursorId, DriverError, Engine, ExecuteRequest,
    ObjectInfo, ObjectKind, PrimitiveType, Row, SchemaScope, SchemaSnapshot, SchemaTree,
    ServerInfo, TxId, TxMode, TypeCategory, TypeRef, Value,
};
use tiberius::{AuthMethod, Client, ColumnType, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const ROW_BATCH_SIZE: usize = 128;

type MssqlConn = Client<Compat<TcpStream>>;

pub struct MssqlDriver {
    inner: Arc<MssqlInner>,
}

struct MssqlInner {
    conns: Mutex<HashMap<u64, MssqlConn>>,
    conn_id: IdCounter,
    tx_id: IdCounter,
    cursor_id: IdCounter,
}

impl MssqlDriver {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MssqlInner {
                conns: Mutex::new(HashMap::new()),
                conn_id: IdCounter::new(),
                tx_id: IdCounter::new(),
                cursor_id: IdCounter::new(),
            }),
        }
    }

    async fn take_conn(&self, c: &ConnHandle) -> Result<MssqlConn, DriverError> {
        self.inner
            .conns
            .lock()
            .await
            .remove(&c.id())
            .ok_or_else(|| DriverError::new(Code::ConnectionFailed, "no conn for handle"))
    }

    async fn put_conn(&self, c: &ConnHandle, conn: MssqlConn) {
        self.inner.conns.lock().await.insert(c.id(), conn);
    }
}

impl Default for MssqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MssqlDriver {
    fn engine(&self) -> Engine {
        Engine::SqlServer
    }

    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        let mut config = Config::new();
        config.host(&spec.host);
        if let Some(port) = spec.port {
            config.port(port);
        }
        if let Some(database) = &spec.database {
            config.database(database);
        }
        config.application_name("sift");
        config.authentication(AuthMethod::sql_server(
            spec.user.clone(),
            spec.password.clone().unwrap_or_default(),
        ));

        if let Some(sift_protocol::EngineConnectionSpec::SqlServer(ms)) = &spec.engine_specific {
            if let Some(encrypt) = ms.encrypt {
                config.encryption(if encrypt {
                    EncryptionLevel::Required
                } else {
                    EncryptionLevel::Off
                });
            }
            if ms.trust_server_certificate.unwrap_or(false) {
                config.trust_cert();
            }
        }

        let tcp = TcpStream::connect(config.get_addr())
            .await
            .map_err(io_err)?;
        tcp.set_nodelay(true).map_err(io_err)?;
        let conn = Client::connect(config, tcp.compat_write())
            .await
            .map_err(ms_err)?;
        let id = self.inner.conn_id.next();
        self.inner.conns.lock().await.insert(id, conn);
        Ok(ConnHandle::new(id, Engine::SqlServer))
    }

    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        let result = async {
            let row = conn
                .query(
                    "SELECT @@VERSION AS version, DB_NAME() AS database_name, SUSER_SNAME() AS user_name",
                    &[],
                )
                .await
                .map_err(ms_err)?
                .into_row()
                .await
                .map_err(ms_err)?
                .ok_or_else(|| DriverError::new(Code::DriverInternal, "ping returned no row"))?;
            Ok::<_, DriverError>(ServerInfo {
                engine: Engine::SqlServer,
                server_version: row.try_get::<&str, _>(0).map_err(ms_err)?.unwrap_or_default().to_string(),
                current_database: row.try_get::<&str, _>(1).map_err(ms_err)?.unwrap_or_default().to_string(),
                current_user: row.try_get::<&str, _>(2).map_err(ms_err)?.unwrap_or_default().to_string(),
            })
        }
        .await;
        self.put_conn(&c, conn).await;
        result
    }

    async fn schema(
        &self,
        c: ConnHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        let result = mssql_schema(&mut conn, scope).await;
        self.put_conn(&c, conn).await;
        result
    }

    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        conn.execute("BEGIN TRANSACTION", &[])
            .await
            .map_err(ms_err)?;
        let tx_id = TxId::new(self.inner.tx_id.next());
        self.put_conn(&c, conn).await;
        Ok(TxHandle::new(tx_id, c, mode))
    }

    async fn commit(&self, t: TxHandle) -> Result<(), DriverError> {
        let mut conn = self.take_conn(&t.conn).await?;
        let result = conn
            .execute("COMMIT TRANSACTION", &[])
            .await
            .map_err(ms_err);
        self.put_conn(&t.conn, conn).await;
        result.map(|_| ())
    }

    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError> {
        let mut conn = self.take_conn(&t.conn).await?;
        let result = conn
            .execute("ROLLBACK TRANSACTION", &[])
            .await
            .map_err(ms_err);
        self.put_conn(&t.conn, conn).await;
        result.map(|_| ())
    }

    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        let conn = self.take_conn(&c).await?;
        let cursor_id = CursorId::new(self.inner.cursor_id.next());
        let (tx, rx) = mpsc::channel(1);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            run_query(inner, c.id(), conn, cursor_id, req, tx).await;
        });
        Ok(ResultSetStream::with_cursor_mode(cursor_id, rx, false))
    }

    async fn cancel(&self, _c: ConnHandle, _cursor: CursorId) -> Result<(), DriverError> {
        Err(DriverError::new(
            Code::UnsupportedForEngine,
            "SQL Server cancel/attention is not wired yet",
        )
        .with_engine(Engine::SqlServer))
    }

    async fn close(&self, c: ConnHandle) -> Result<(), DriverError> {
        self.inner.conns.lock().await.remove(&c.id());
        Ok(())
    }

    fn as_mssql(&self) -> Option<&dyn MssqlExt> {
        Some(self)
    }
}

#[async_trait]
impl MssqlExt for MssqlDriver {
    async fn use_database(&self, c: ConnHandle, db: &str) -> Result<(), DriverError> {
        validate_ident(db)?;
        let mut conn = self.take_conn(&c).await?;
        let sql = format!("USE [{db}]");
        let result = conn.execute(sql, &[]).await.map_err(ms_err);
        self.put_conn(&c, conn).await;
        result.map(|_| ())
    }

    async fn bulk_insert(
        &self,
        _c: ConnHandle,
        _op: sift_driver_api::BulkOp,
    ) -> Result<sift_driver_api::BulkResult, DriverError> {
        Err(DriverError::new(
            Code::UnsupportedForEngine,
            "bulk insert not wired yet",
        ))
    }

    async fn set_mars(&self, _c: ConnHandle, _enabled: bool) -> Result<(), DriverError> {
        Err(DriverError::new(
            Code::UnsupportedForEngine,
            "MARS toggle not wired yet",
        ))
    }

    async fn savepoint(
        &self,
        t: &TxHandle,
        name: &str,
    ) -> Result<sift_driver_api::MssqlSavepoint, DriverError> {
        validate_ident(name)?;
        let mut conn = self.take_conn(&t.conn).await?;
        let sql = format!("SAVE TRANSACTION [{name}]");
        let result = conn.execute(sql, &[]).await.map_err(ms_err);
        self.put_conn(&t.conn, conn).await;
        result?;
        Ok(sift_driver_api::MssqlSavepoint {
            tx: t.tx_id,
            conn: t.conn.clone(),
            name: name.to_string(),
        })
    }

    async fn rollback_to(&self, sp: sift_driver_api::MssqlSavepoint) -> Result<(), DriverError> {
        validate_ident(&sp.name)?;
        let mut conn = self.take_conn(&sp.conn).await?;
        let sql = format!("ROLLBACK TRANSACTION [{}]", sp.name);
        let result = conn.execute(sql, &[]).await.map_err(ms_err);
        self.put_conn(&sp.conn, conn).await;
        result.map(|_| ())
    }
}

async fn run_query(
    inner: Arc<MssqlInner>,
    conn_id: u64,
    mut conn: MssqlConn,
    cursor_id: CursorId,
    req: ExecuteRequest,
    tx: mpsc::Sender<sift_protocol::Page>,
) {
    let result = async {
        if !req.params.is_empty() {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "SQL Server dynamic parameters not wired yet",
            ));
        }
        let mut stream = conn.query(req.sql, &[]).await.map_err(ms_err)?;
        let mut batch = Vec::with_capacity(ROW_BATCH_SIZE);
        while let Some(item) = stream.next().await {
            match item.map_err(ms_err)? {
                QueryItem::Metadata(meta) => {
                    if !batch.is_empty() {
                        let rows = std::mem::take(&mut batch);
                        if tx.send(sift_protocol::Page::Rows(rows)).await.is_err() {
                            return Ok::<_, DriverError>(());
                        }
                    }
                    let columns = meta.columns().iter().map(ms_col).collect();
                    if tx
                        .send(sift_protocol::Page::NextResult { columns })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                QueryItem::Row(row) => {
                    batch.push(ms_row(&row));
                    if batch.len() >= ROW_BATCH_SIZE {
                        let rows = std::mem::take(&mut batch);
                        if tx.send(sift_protocol::Page::Rows(rows)).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
        if !batch.is_empty() {
            let _ = tx.send(sift_protocol::Page::Rows(batch)).await;
        }
        let _ = tx
            .send(sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            })
            .await;
        Ok(())
    }
    .await;

    if let Err(error) = result {
        let _ = tx.send(sift_protocol::Page::Error { error }).await;
    }
    inner.conns.lock().await.insert(conn_id, conn);
    tracing::debug!(%cursor_id, conn_id, "sqlserver query finished");
}

async fn mssql_schema(
    conn: &mut MssqlConn,
    scope: SchemaScope,
) -> Result<SchemaSnapshot, DriverError> {
    let mut snapshot = SchemaSnapshot::empty(scope.clone());
    let rows = conn
        .query(
            "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES ORDER BY TABLE_SCHEMA, TABLE_NAME",
            &[],
        )
        .await
        .map_err(ms_err)?
        .into_first_result()
        .await
        .map_err(ms_err)?;

    let mut by_schema: std::collections::BTreeMap<String, Vec<ObjectInfo>> = Default::default();
    for row in rows {
        let schema = row
            .try_get::<&str, _>(0)
            .map_err(ms_err)?
            .unwrap_or("dbo")
            .to_string();
        let name = row
            .try_get::<&str, _>(1)
            .map_err(ms_err)?
            .unwrap_or_default()
            .to_string();
        let table_type = row
            .try_get::<&str, _>(2)
            .map_err(ms_err)?
            .unwrap_or_default();
        let kind = if table_type == "VIEW" {
            ObjectKind::View
        } else {
            ObjectKind::Table
        };
        by_schema
            .entry(schema)
            .or_default()
            .push(ObjectInfo::new(name, kind));
    }
    snapshot.trees.push(sift_protocol::CatalogTree {
        name: "default".to_string(),
        schemas: by_schema
            .into_iter()
            .map(|(name, objects)| SchemaTree { name, objects })
            .collect(),
    });
    Ok(snapshot)
}

fn ms_col(col: &tiberius::Column) -> ColumnMetadata {
    ColumnMetadata {
        name: col.name().to_string(),
        type_ref: ms_type_ref(col.column_type()),
        nullable: sift_protocol::Nullability::Unknown,
        auto_increment: false,
        primary_key: false,
        facets: sift_protocol::EngineColumnFacets {
            postgres: None,
            sql_server: Some(sift_protocol::MssqlColumnFacets {
                tds_type: Some(format!("{:?}", col.column_type())),
                collation: None,
                max_length: None,
            }),
        },
    }
}

fn ms_row(row: &tiberius::Row) -> Row {
    let mut values = Vec::with_capacity(row.len());
    for idx in 0..row.len() {
        values.push(ms_value(row, idx));
    }
    Row::new(values)
}

fn ms_value(row: &tiberius::Row, idx: usize) -> Value {
    let ty = row.columns()[idx].column_type();
    match ty {
        ColumnType::Bit | ColumnType::Bitn => {
            row.try_get::<bool, _>(idx).ok().flatten().map(Value::Bool)
        }
        ColumnType::Int1 => row
            .try_get::<u8, _>(idx)
            .ok()
            .flatten()
            .map(|v| Value::Int16(v as i16)),
        ColumnType::Int2 => row.try_get::<i16, _>(idx).ok().flatten().map(Value::Int16),
        ColumnType::Int4 | ColumnType::Intn => {
            row.try_get::<i32, _>(idx).ok().flatten().map(Value::Int32)
        }
        ColumnType::Int8 => row.try_get::<i64, _>(idx).ok().flatten().map(Value::Int64),
        ColumnType::Float4 | ColumnType::Floatn => row
            .try_get::<f32, _>(idx)
            .ok()
            .flatten()
            .map(Value::Float32),
        ColumnType::Float8 => row
            .try_get::<f64, _>(idx)
            .ok()
            .flatten()
            .map(Value::Float64),
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => row
            .try_get::<&[u8], _>(idx)
            .ok()
            .flatten()
            .map(|v| Value::Blob(v.to_vec())),
        ColumnType::Guid => row
            .try_get::<uuid::Uuid, _>(idx)
            .ok()
            .flatten()
            .map(Value::Uuid),
        ColumnType::Daten => row
            .try_get::<chrono::NaiveDate, _>(idx)
            .ok()
            .flatten()
            .map(Value::Date),
        ColumnType::Timen => row
            .try_get::<chrono::NaiveTime, _>(idx)
            .ok()
            .flatten()
            .map(Value::Time),
        ColumnType::Datetime
        | ColumnType::Datetime2
        | ColumnType::Datetime4
        | ColumnType::Datetimen => row
            .try_get::<chrono::NaiveDateTime, _>(idx)
            .ok()
            .flatten()
            .map(Value::Timestamp),
        _ => row
            .try_get::<&str, _>(idx)
            .ok()
            .flatten()
            .map(|v| Value::Text(v.to_string())),
    }
    .unwrap_or(Value::Null)
}

fn ms_type_ref(ty: ColumnType) -> TypeRef {
    let primitive = match ty {
        ColumnType::Bit | ColumnType::Bitn => Some(PrimitiveType::Bool),
        ColumnType::Int1 | ColumnType::Int2 => Some(PrimitiveType::Int16),
        ColumnType::Int4 | ColumnType::Intn => Some(PrimitiveType::Int32),
        ColumnType::Int8 => Some(PrimitiveType::Int64),
        ColumnType::Float4 | ColumnType::Floatn => Some(PrimitiveType::Float32),
        ColumnType::Float8 => Some(PrimitiveType::Float64),
        ColumnType::Decimaln | ColumnType::Numericn | ColumnType::Money | ColumnType::Money4 => {
            Some(PrimitiveType::Decimal)
        }
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => {
            Some(PrimitiveType::Blob)
        }
        ColumnType::Daten => Some(PrimitiveType::Date),
        ColumnType::Timen => Some(PrimitiveType::Time),
        ColumnType::Datetime
        | ColumnType::Datetime2
        | ColumnType::Datetime4
        | ColumnType::Datetimen => Some(PrimitiveType::Timestamp),
        ColumnType::Guid => Some(PrimitiveType::Uuid),
        ColumnType::Xml => Some(PrimitiveType::Text),
        ColumnType::BigVarChar
        | ColumnType::BigChar
        | ColumnType::NVarchar
        | ColumnType::NChar
        | ColumnType::Text
        | ColumnType::NText => Some(PrimitiveType::Text),
        _ => None,
    };
    primitive
        .map(TypeRef::Primitive)
        .unwrap_or_else(|| TypeRef::Engine {
            engine: Engine::SqlServer,
            name: format!("{ty:?}"),
            category: TypeCategory::Other,
        })
}

fn validate_ident(name: &str) -> Result<(), DriverError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(DriverError::new(
            Code::InvalidParameterValue,
            "invalid identifier",
        ))
    }
}

fn io_err(e: std::io::Error) -> DriverError {
    DriverError::new(Code::ConnectionFailed, e.to_string()).with_engine(Engine::SqlServer)
}

fn ms_err(e: tiberius::error::Error) -> DriverError {
    DriverError::new(Code::DriverInternal, e.to_string()).with_engine(Engine::SqlServer)
}
