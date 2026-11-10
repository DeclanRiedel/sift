//! Faults at the host's observation boundary; all database work uses the real Driver.
use sift_driver_api::{ConnHandle, Driver, MssqlExt, PgExt, ResultSetStream, SqliteExt, TxHandle};
use sift_protocol::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

pub struct NativeFaults {
    inner: Arc<dyn Driver>,
    lost_commit: AtomicBool,
    count_target: Mutex<Option<(String, Option<u64>)>>,
    executed: AtomicUsize,
    native_update_observed: Arc<AtomicBool>,
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
    rollback_error: Mutex<Option<(String, Option<String>)>>,
}
impl NativeFaults {
    pub fn new(inner: Arc<dyn Driver>) -> Self {
        Self {
            inner,
            lost_commit: false.into(),
            count_target: Mutex::new(None),
            executed: 0.into(),
            native_update_observed: Arc::new(AtomicBool::new(false)),
            commits: 0.into(),
            rollbacks: 0.into(),
            rollback_error: Mutex::new(None),
        }
    }
    pub fn lose_next_commit_ack(&self) {
        self.lost_commit.store(true, Ordering::SeqCst);
    }
    pub fn replace_next_update_count(&self, table: &str, count: Option<u64>) {
        self.native_update_observed.store(false, Ordering::SeqCst);
        *self.count_target.lock().unwrap() = Some((table.into(), count));
    }
    pub fn native_update_observed(&self) -> bool {
        self.native_update_observed.load(Ordering::SeqCst)
    }
    pub fn rollback_error(&self) -> Option<(String, Option<String>)> {
        self.rollback_error.lock().unwrap().clone()
    }
    pub fn counters(&self) -> (usize, usize, usize) {
        (
            self.executed.load(Ordering::SeqCst),
            self.commits.load(Ordering::SeqCst),
            self.rollbacks.load(Ordering::SeqCst),
        )
    }
    pub fn fault_consumed(&self) -> bool {
        !self.lost_commit.load(Ordering::SeqCst) && self.count_target.lock().unwrap().is_none()
    }
}
#[async_trait::async_trait]
impl Driver for NativeFaults {
    fn engine(&self) -> Engine {
        self.inner.engine()
    }
    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        self.inner.open(spec).await
    }
    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError> {
        self.inner.ping(c).await
    }
    async fn schema(
        &self,
        c: ConnHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        self.inner.schema(c, scope).await
    }
    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError> {
        self.inner.begin(c, mode).await
    }
    async fn commit(&self, tx: TxHandle) -> Result<(), DriverError> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        let lose = self.lost_commit.swap(false, Ordering::SeqCst);
        self.inner.commit(tx).await?;
        if lose {
            return Err(DriverError::new(
                Code::ConnectionFailed,
                "PRIVATE-NATIVE-COMMIT-ACK-LOST",
            ));
        }
        Ok(())
    }
    async fn rollback(&self, tx: TxHandle) -> Result<(), DriverError> {
        self.rollbacks.fetch_add(1, Ordering::SeqCst);
        let result = self.inner.rollback(tx).await;
        *self.rollback_error.lock().unwrap() = result.as_ref().err().map(|error| {
            let code = if matches!(error.code, Code::Other { .. }) {
                "other".to_owned()
            } else {
                serde_json::to_value(&error.code)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("code")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "driver_internal".into())
            };
            (code, error.native_code.clone())
        });
        result
    }
    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        let replacement = {
            let mut target = self.count_target.lock().unwrap();
            if req
                .sql
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("UPDATE ")
                && target
                    .as_ref()
                    .is_some_and(|(name, _)| req.sql.contains(name))
            {
                target.take().map(|(_, count)| count)
            } else {
                None
            }
        };
        let mut stream = self.inner.execute(c, req).await?;
        if let Some(count) = replacement {
            if stream.affected_rows == Some(1) {
                self.native_update_observed.store(true, Ordering::SeqCst);
            }
            let observed = self.native_update_observed.clone();
            let (send, recv) = tokio::sync::mpsc::channel(4);
            let mut rows = stream.rows;
            tokio::spawn(async move {
                while let Some(mut page) = rows.recv().await {
                    if let Page::Done { affected_rows, .. } = &mut page {
                        if *affected_rows == Some(1) {
                            observed.store(true, Ordering::SeqCst);
                        }
                        *affected_rows = count;
                    }
                    if send.send(page).await.is_err() {
                        break;
                    }
                }
            });
            stream.rows = recv;
            stream.affected_rows = count;
        }
        Ok(stream)
    }
    async fn cancel(&self, c: ConnHandle, cursor: CursorId) -> Result<(), DriverError> {
        self.inner.cancel(c, cursor).await
    }
    async fn close(&self, c: ConnHandle) -> Result<(), DriverError> {
        self.inner.close(c).await
    }
    fn as_pg(&self) -> Option<&dyn PgExt> {
        self.inner.as_pg()
    }
    fn as_mssql(&self) -> Option<&dyn MssqlExt> {
        self.inner.as_mssql()
    }
    fn as_sqlite(&self) -> Option<&dyn SqliteExt> {
        self.inner.as_sqlite()
    }
}
