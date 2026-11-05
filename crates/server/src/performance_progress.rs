//! Row-free progress on existing active registrations. Reads never call a driver.
use super::*;
use sift_protocol::{
    BenchmarkOutcome, BenchmarkProgressCounters, BenchmarkRequest, OperationKind,
    PerformanceProgress, PerformanceRunKind, PerformanceStage,
};

pub(super) struct ProgressState {
    sql: Arc<str>,
    additional_sql: Option<Arc<str>>,
    original: PerformanceRunKind,
    paired: bool,
    started: Instant,
    snapshot: Mutex<PerformanceProgress>,
}
impl ProgressState {
    pub(super) fn benchmark(request: &BenchmarkRequest) -> Self {
        Self::new(
            request.run_id,
            PerformanceRunKind::Benchmark,
            &request.sql,
            Some(BenchmarkProgressCounters {
                requested_warmups: request.warmups,
                requested_iterations: request.iterations,
                finished_warmups: 0,
                finished_iterations: 0,
                last_outcome: None,
            }),
        )
    }
    pub(super) fn paired(request: &sift_protocol::BenchmarkPairRequest) -> Self {
        let mut state = Self::new(
            request.run_id,
            PerformanceRunKind::Benchmark,
            &request.baseline.sql,
            Some(BenchmarkProgressCounters {
                requested_warmups: request.limits.warmups * 2,
                requested_iterations: request.limits.iterations * 2,
                finished_warmups: 0,
                finished_iterations: 0,
                last_outcome: None,
            }),
        );
        state.additional_sql = Some(request.candidate.sql.as_str().into());
        state.paired = true;
        state
    }
    pub(super) fn profile(run: uuid::Uuid, sql: &str) -> Self {
        Self::new(run, PerformanceRunKind::Profile, sql, None)
    }
    fn new(
        run_id: uuid::Uuid,
        kind: PerformanceRunKind,
        sql: &str,
        benchmark: Option<BenchmarkProgressCounters>,
    ) -> Self {
        Self {
            sql: sql.into(),
            additional_sql: None,
            original: kind,
            paired: false,
            started: Instant::now(),
            snapshot: Mutex::new(PerformanceProgress {
                run_id,
                kind,
                stage: PerformanceStage::SettingUp,
                elapsed_ms: 0,
                benchmark,
                cancellation_requested: false,
            }),
        }
    }
    fn update(&self, stage: PerformanceStage, attempt: Option<(bool, BenchmarkOutcome)>) {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.stage = stage;
        if let (Some(counters), Some((warmup, outcome))) = (&mut snapshot.benchmark, attempt) {
            if warmup {
                counters.finished_warmups += 1;
            } else {
                counters.finished_iterations += 1;
            }
            counters.last_outcome = Some(outcome);
        }
    }
    fn read(&self, cancellation_requested: bool) -> PerformanceProgress {
        let mut snapshot = self.snapshot.lock().unwrap().clone();
        snapshot.elapsed_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        snapshot.cancellation_requested = cancellation_requested;
        snapshot
    }
}

impl SessionStore {
    pub fn performance_progress(
        &self,
        session: SessionId,
        source: ConnectionId,
        run: uuid::Uuid,
        mode: PerformanceRunKind,
    ) -> ApiResult<Option<PerformanceProgress>> {
        // Recheck even for inactive identities, without disclosing another run.
        self.authorize_connection_operation(
            session,
            source,
            OperationKind::ReadPerformanceProgress,
            None,
            &[],
        )?;
        let active = match mode {
            PerformanceRunKind::Benchmark => self
                .inner
                .benchmarks
                .get(&(session, source))
                .filter(|active| active.run_id == run)
                .map(|active| (active.progress.clone(), active.cancellation.is_cancelled())),
            PerformanceRunKind::Profile => self
                .inner
                .profiles
                .get(&(session, source))
                .filter(|active| active.run_id == run)
                .map(|active| (active.progress.clone(), active.cancellation.is_cancelled())),
        };
        let Some((progress, cancelled)) = active else {
            return Ok(None);
        };
        let original = if progress.paired {
            OperationKind::BenchmarkPair
        } else {
            match progress.original {
                PerformanceRunKind::Benchmark => OperationKind::BenchmarkQuery,
                PerformanceRunKind::Profile => OperationKind::ProfileQuery,
            }
        };
        for kind in [
            OperationKind::ReadPerformanceProgress,
            original,
            OperationKind::ExecuteQuery,
        ] {
            self.authorize_connection_operation(session, source, kind, Some(&progress.sql), &[])?;
            if let Some(sql) = &progress.additional_sql {
                self.authorize_connection_operation(session, source, kind, Some(sql), &[])?;
            }
        }
        Ok(Some(progress.read(cancelled)))
    }

    pub(super) fn update_performance_progress(
        &self,
        session: SessionId,
        source: ConnectionId,
        run: uuid::Uuid,
        mode: PerformanceRunKind,
        stage: PerformanceStage,
        attempt: Option<(bool, BenchmarkOutcome)>,
    ) {
        match mode {
            PerformanceRunKind::Benchmark => {
                if let Some(active) = self
                    .inner
                    .benchmarks
                    .get(&(session, source))
                    .filter(|a| a.run_id == run)
                {
                    active.progress.update(stage, attempt);
                }
            }
            PerformanceRunKind::Profile => {
                if let Some(active) = self
                    .inner
                    .profiles
                    .get(&(session, source))
                    .filter(|a| a.run_id == run)
                {
                    active.progress.update(stage, attempt);
                }
            }
        }
    }
}
