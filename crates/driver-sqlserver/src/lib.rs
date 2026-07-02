//! `sift-driver-sqlserver` — SQL Server via tiberius (ADR-003).
//!
//! Per PHASE0.md sequencing: this crate is **stubbed until step 15**.
//! Postgres ships first (steps 7–14) so server-substrate bugs aren't
//! confounded with tiberius-driver bugs; tiberius lands as the trait's
//! fast-follow stress test inside Phase 0. The trait is not "public" until
//! this impl passes end-to-end (PHASE0.md DoD).
//!
//! All methods return `UnsupportedForEngine` until the real impl ships.

use async_trait::async_trait;
use sift_driver_api::{ConnHandle, Driver, MssqlExt, ResultSetStream, TxHandle};
use sift_protocol::{
    Code, ConnectionSpec, CursorId, DriverError, Engine, ExecuteRequest, SchemaScope,
    SchemaSnapshot, ServerInfo, TxMode,
};

/// Stub driver. Holds no state until the real impl lands.
pub struct MssqlDriver;

impl MssqlDriver {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MssqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

fn unsupported() -> DriverError {
    DriverError::new(
        Code::UnsupportedForEngine,
        "SQL Server driver not yet implemented (PHASE0.md step 15)",
    )
    .with_engine(Engine::SqlServer)
}

#[async_trait]
impl Driver for MssqlDriver {
    fn engine(&self) -> Engine {
        Engine::SqlServer
    }

    async fn open(&self, _spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        Err(unsupported())
    }

    async fn ping(&self, _c: ConnHandle) -> Result<ServerInfo, DriverError> {
        Err(unsupported())
    }

    async fn schema(
        &self,
        _c: ConnHandle,
        _scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        Err(unsupported())
    }

    async fn begin(&self, _c: ConnHandle, _mode: TxMode) -> Result<TxHandle, DriverError> {
        Err(unsupported())
    }

    async fn commit(&self, _t: TxHandle) -> Result<(), DriverError> {
        Err(unsupported())
    }

    async fn rollback(&self, _t: TxHandle) -> Result<(), DriverError> {
        Err(unsupported())
    }

    async fn execute(
        &self,
        _c: ConnHandle,
        _req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        Err(unsupported())
    }

    async fn cancel(&self, _c: ConnHandle, _cursor: CursorId) -> Result<(), DriverError> {
        Err(unsupported())
    }

    async fn close(&self, _c: ConnHandle) -> Result<(), DriverError> {
        Err(unsupported())
    }

    fn as_mssql(&self) -> Option<&dyn MssqlExt> {
        Some(self)
    }
}

#[async_trait]
impl MssqlExt for MssqlDriver {
    async fn use_database(&self, _c: ConnHandle, _db: &str) -> Result<(), DriverError> {
        Err(unsupported())
    }

    async fn bulk_insert(
        &self,
        _c: ConnHandle,
        _op: sift_driver_api::BulkOp,
    ) -> Result<sift_driver_api::BulkResult, DriverError> {
        Err(unsupported())
    }

    async fn set_mars(&self, _c: ConnHandle, _enabled: bool) -> Result<(), DriverError> {
        Err(unsupported())
    }

    async fn savepoint(
        &self,
        _t: &TxHandle,
        _name: &str,
    ) -> Result<sift_driver_api::MssqlSavepoint, DriverError> {
        Err(unsupported())
    }

    async fn rollback_to(&self, _sp: sift_driver_api::MssqlSavepoint) -> Result<(), DriverError> {
        Err(unsupported())
    }
}
