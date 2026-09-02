//! Best-effort PostgreSQL progress sampling on a separate pooled connection.

use std::time::Duration;

use sift_driver_api::{ConnHandle, NativeProgressStream};
use sift_protocol::{
    Code, CursorId, DriverError, NativeExecutionProgress, NativeProgressSource,
    MAX_NATIVE_PROGRESS_PHASE_BYTES,
};

use crate::conn::PooledConn;
use crate::PgDriver;

const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
const MONITOR_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PgProgressKind {
    Vacuum,
    Analyze,
    CreateIndex,
    Cluster,
    Copy,
}

pub(crate) fn classify(sql: &str) -> Option<PgProgressKind> {
    let normalized = leading_sql(sql).to_ascii_uppercase();
    if normalized.starts_with("VACUUM") {
        Some(PgProgressKind::Vacuum)
    } else if normalized.starts_with("ANALYZE") {
        Some(PgProgressKind::Analyze)
    } else if normalized.starts_with("CREATE INDEX")
        || normalized.starts_with("CREATE UNIQUE INDEX")
        || normalized.starts_with("REINDEX")
    {
        Some(PgProgressKind::CreateIndex)
    } else if normalized.starts_with("CLUSTER") {
        Some(PgProgressKind::Cluster)
    } else if normalized.starts_with("COPY") {
        Some(PgProgressKind::Copy)
    } else {
        None
    }
}

pub(crate) fn observe(
    driver: &PgDriver,
    connection: &ConnHandle,
    cursor: CursorId,
) -> Result<Option<NativeProgressStream>, DriverError> {
    let Some(entry) = driver.inner.cursors.get(&cursor.0) else {
        return Ok(None);
    };
    if entry.conn_id != connection.id() {
        return Err(DriverError::new(Code::CursorNotFound, "cursor not active"));
    }
    let Some(kind) = entry.progress_kind else {
        return Ok(None);
    };
    let backend_pid = entry.backend_pid;
    if backend_pid <= 0 {
        return Ok(None);
    }
    drop(entry);
    let Some(spec) = driver.inner.spec_for(connection.id()) else {
        return Ok(None);
    };
    let Ok(permit) = driver.inner.progress_slots.clone().try_acquire_owned() else {
        return Ok(None);
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let driver = driver.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let Ok(Ok(monitor)) =
            tokio::time::timeout(MONITOR_TIMEOUT, driver.open_internal(&spec)).await
        else {
            return;
        };
        sample_loop(driver, monitor, cursor, backend_pid, kind, sender).await;
    });
    Ok(Some(NativeProgressStream { updates: receiver }))
}

async fn sample_loop(
    driver: PgDriver,
    monitor: PooledConn,
    cursor: CursorId,
    backend_pid: i32,
    kind: PgProgressKind,
    sender: tokio::sync::mpsc::Sender<NativeExecutionProgress>,
) {
    loop {
        if sender.is_closed() || !driver.inner.cursors.contains_key(&cursor.0) {
            return;
        }
        let sample = tokio::time::timeout(
            MONITOR_TIMEOUT,
            monitor.query_opt(query(kind), &[&backend_pid]),
        )
        .await;
        let Ok(Ok(Some(row))) = sample else {
            return;
        };
        let basis_points = row.try_get::<_, i64>(0).unwrap_or(0).clamp(0, 10_000) as u16;
        let phase = row.try_get::<_, String>(1).ok().map(bounded_phase);
        if sender
            .send(NativeExecutionProgress {
                source: NativeProgressSource::PostgresStatistics,
                basis_points,
                phase,
                estimated_remaining_ms: None,
            })
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

fn query(kind: PgProgressKind) -> &'static str {
    match kind {
        PgProgressKind::Vacuum => "SELECT CASE WHEN heap_blks_total > 0 THEN LEAST(10000, heap_blks_scanned * 10000 / heap_blks_total) ELSE 0 END::bigint, phase::text FROM pg_catalog.pg_stat_progress_vacuum WHERE pid = $1",
        PgProgressKind::Analyze => "SELECT CASE WHEN sample_blks_total > 0 THEN LEAST(10000, sample_blks_scanned * 10000 / sample_blks_total) ELSE 0 END::bigint, phase::text FROM pg_catalog.pg_stat_progress_analyze WHERE pid = $1",
        PgProgressKind::CreateIndex => "SELECT CASE WHEN blocks_total > 0 THEN LEAST(10000, blocks_done * 10000 / blocks_total) ELSE 0 END::bigint, phase::text FROM pg_catalog.pg_stat_progress_create_index WHERE pid = $1",
        PgProgressKind::Cluster => "SELECT CASE WHEN heap_blks_total > 0 THEN LEAST(10000, heap_blks_scanned * 10000 / heap_blks_total) ELSE 0 END::bigint, phase::text FROM pg_catalog.pg_stat_progress_cluster WHERE pid = $1",
        PgProgressKind::Copy => "SELECT CASE WHEN bytes_total > 0 THEN LEAST(10000, bytes_processed * 10000 / bytes_total) ELSE 0 END::bigint, command::text FROM pg_catalog.pg_stat_progress_copy WHERE pid = $1",
    }
}

fn bounded_phase(mut phase: String) -> String {
    let mut end = MAX_NATIVE_PROGRESS_PHASE_BYTES.min(phase.len());
    while !phase.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    phase.truncate(end);
    phase
}

fn leading_sql(mut sql: &str) -> &str {
    loop {
        sql = sql.trim_start();
        if let Some(rest) = sql.strip_prefix("--") {
            sql = rest.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(rest) = sql.strip_prefix("/*") {
            sql = rest.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            return sql;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_supported_maintenance_commands_are_classified() {
        assert_eq!(
            classify("/* job */ VACUUM public.users"),
            Some(PgProgressKind::Vacuum)
        );
        assert_eq!(
            classify("CREATE UNIQUE INDEX x ON t (id)"),
            Some(PgProgressKind::CreateIndex)
        );
        assert_eq!(classify("COPY t TO STDOUT"), Some(PgProgressKind::Copy));
        assert_eq!(classify("SELECT pg_sleep(2)"), None);
        assert_eq!(classify("UPDATE t SET secret = 'VACUUM'"), None);
    }

    #[test]
    fn monitor_queries_contain_no_user_sql_or_credentials() {
        for kind in [
            PgProgressKind::Vacuum,
            PgProgressKind::Analyze,
            PgProgressKind::CreateIndex,
            PgProgressKind::Cluster,
            PgProgressKind::Copy,
        ] {
            let sql = query(kind);
            assert!(sql.contains("WHERE pid = $1"));
            assert!(!sql.contains("password"));
        }
    }
}
