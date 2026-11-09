//! Concurrent suite supervision. Observers never own native task cancellation.
use super::*;
use sift_protocol::{
    BenchmarkLoadConfig, BenchmarkLoadContext, BenchmarkLoadTrace, BenchmarkLoadWindow,
};
#[path = "benchmark_load_worker.rs"]
mod worker;
pub(super) use worker::WorkerSource;
use worker::{Cleanup, Control, PrivateWorker, QueryInput, QueryObservation};

struct Node {
    index: u16,
    worker: Arc<PrivateWorker>,
}

struct Record {
    case: u16,
    worker: u16,
    ordinal: u32,
    warmup: bool,
    start_ns: u64,
    wait_ns: u64,
    sample: Option<(BenchmarkSample, BenchmarkLoadTrace)>,
}

struct Ledger {
    next_start: tokio::time::Instant,
    case_deadlines: Vec<Option<tokio::time::Instant>>,
    counts: Vec<u32>,
    records: Vec<Record>,
}

struct Shared {
    source: Arc<WorkerSource>,
    requests: Arc<Vec<BenchmarkRequest>>,
    control: Control,
    config: BenchmarkLoadConfig,
    origin: tokio::time::Instant,
    ledger: tokio::sync::Mutex<Ledger>,
    stopped: Mutex<Option<&'static str>>,
}

impl Shared {
    fn stop(&self, reason: &'static str) {
        self.stopped.lock().unwrap().get_or_insert(reason);
        self.control.cancellation.cancel();
    }

    async fn admit(&self, worker: u16, case: u16, warmup: bool) -> Option<(usize, QueryInput)> {
        let waiting = self.origin.elapsed();
        let request = &self.requests[case as usize];
        let (mut ledger, deadline) = loop {
            let ledger = tokio::select! {
                biased;
                _ = self.control.cancellation.cancelled() => return None,
                _ = tokio::time::sleep_until(self.control.deadline) => return None,
                ledger = self.ledger.lock() => ledger,
            };
            let deadline = ledger.case_deadlines[case as usize].unwrap_or(self.control.deadline);
            if self.control.check().is_err() || tokio::time::Instant::now() >= deadline {
                self.stop("Stopped: shared or per-case budget exhausted");
                return None;
            }
            if ledger.next_start <= tokio::time::Instant::now() {
                break (ledger, deadline);
            }
            let next_start = ledger.next_start;
            drop(ledger);
            tokio::select! {
                biased;
                _ = self.control.cancellation.cancelled() => return None,
                _ = tokio::time::sleep_until(deadline) => { self.stop("Stopped: shared or per-case budget exhausted"); return None; },
                _ = tokio::time::sleep_until(next_start) => {},
            }
        };
        if self.control.check().is_err() || tokio::time::Instant::now() >= deadline {
            self.stop("Stopped: cancellation or sampling budget exhausted");
            return None;
        }
        if self.source.check(&request.sql).is_err() {
            self.stop("Stopped: source generation or current authority changed");
            return None;
        }
        let quota = match self
            .source
            .store
            .reserve_query_resources(&self.source.entry)
        {
            Ok(quota) => quota,
            Err(_) => {
                self.stop("Stopped: query/cursor resource admission refused");
                return None;
            }
        };
        let limits = self.source.store.result_limits();
        let reservation = limits
            .1
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(256 * 1024));
        let bytes = match reservation.and_then(|bytes| self.source.reserve_bytes(bytes).ok()) {
            Some(bytes) => bytes,
            None => {
                self.stop("Stopped: bounded result byte reservation refused");
                return None;
            }
        };
        if self.control.check().is_err() || tokio::time::Instant::now() >= deadline {
            return None;
        }
        let now = tokio::time::Instant::now();
        let case_deadline = *ledger.case_deadlines[case as usize].get_or_insert_with(|| {
            self.control
                .deadline
                .min(now + Duration::from_millis(request.total_budget_ms))
        });
        let start_ns = nanos(now.duration_since(self.origin));
        let position = ledger.records.len();
        let ordinal = ledger.counts[case as usize];
        ledger.counts[case as usize] += 1;
        ledger.records.push(Record {
            case,
            worker,
            ordinal,
            warmup,
            start_ns,
            wait_ns: start_ns.saturating_sub(nanos(waiting)),
            sample: None,
        });
        ledger.next_start = now
            + Duration::from_nanos(
                sift_core::performance::load::minimum_start_spacing_ns(self.config)
                    .expect("validated load rate"),
            );
        self.source.store.update_performance_progress(
            self.source.session,
            self.source.connection,
            self.requests[0].run_id,
            PerformanceRunKind::Benchmark,
            if warmup {
                PerformanceStage::WarmingUp
            } else {
                PerformanceStage::Measuring
            },
            None,
        );
        Some((
            position,
            QueryInput {
                request: ExecuteRequest {
                    sql: request.sql.clone(),
                    params: request.params.clone(),
                    transform: None,
                },
                control: Control {
                    cancellation: self.control.cancellation.clone(),
                    deadline: case_deadline
                        .min(now + Duration::from_millis(request.query_timeout_ms)),
                },
                origin: self.origin,
                start_ns,
                quota,
                bytes,
                limits,
                settings: None,
            },
        ))
    }

    async fn record(
        &self,
        position: usize,
        warmup: bool,
        result: ApiResult<QueryObservation>,
    ) -> bool {
        let (outcome, native_end, first, rows) = match result {
            Ok(observation) => (
                BenchmarkOutcome::Success,
                Some(observation.end_ns),
                observation.first_row_ns,
                Some(observation.rows),
            ),
            Err(ApiError::Driver(error)) if error.code == Code::QueryCanceled => {
                (BenchmarkOutcome::Cancelled, None, None, None)
            }
            Err(ApiError::Driver(error)) if error.code == Code::QueryTimedOut => {
                (BenchmarkOutcome::TimedOut, None, None, None)
            }
            Err(_) => (BenchmarkOutcome::Failed, None, None, None),
        };
        let successful = outcome == BenchmarkOutcome::Success;
        if !successful
            && (warmup
                || self.config.failure_policy == sift_protocol::BenchmarkLoadFailurePolicy::StopRun)
        {
            self.stop("Stopped after an unsuccessful attempt; no retries or replacement workers");
        }
        let mut ledger = self.ledger.lock().await;
        let case = ledger.records[position].case as usize;
        if outcome == BenchmarkOutcome::TimedOut
            && ledger.case_deadlines[case]
                .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            // A healthy peer may already have finished this case and be
            // executing another one. Do not rely on its next admission to
            // discover the exhausted shared case budget under RetireWorker.
            self.stop("Stopped: shared or per-case budget exhausted");
        }
        let end_ns = native_end.unwrap_or_else(|| nanos(self.origin.elapsed()));
        let record = &mut ledger.records[position];
        let elapsed = end_ns.saturating_sub(record.start_ns);
        record.sample = Some((
            BenchmarkSample {
                ordinal: record.ordinal,
                warmup: record.warmup,
                outcome,
                elapsed_ns: elapsed,
                database_execution_ns: None,
                client_elapsed_ns: Some(elapsed),
                first_row_ns: first,
                full_consumption_ns: (outcome == BenchmarkOutcome::Success).then_some(elapsed),
                rows,
            },
            BenchmarkLoadTrace {
                worker: record.worker,
                dispatch_index: position as u16,
                start_ns: record.start_ns,
                end_ns,
                admission_wait_ns: record.wait_ns,
            },
        ));
        self.source.store.update_performance_progress(
            self.source.session,
            self.source.connection,
            self.requests[0].run_id,
            PerformanceRunKind::Benchmark,
            if record.warmup {
                PerformanceStage::WarmingUp
            } else {
                PerformanceStage::Measuring
            },
            Some((record.warmup, outcome)),
        );
        successful
    }
}

async fn observe<T>(
    mut receive: tokio::sync::oneshot::Receiver<ApiResult<T>>,
    control: &Control,
) -> ApiResult<T> {
    tokio::select! {
        biased;
        result = &mut receive => result.map_err(|_| ApiError::Internal("Load native owner failed".into()))?,
        _ = control.cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Load observation cancelled"))),
        _ = tokio::time::sleep_until(control.deadline) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Load observation timed out"))),
    }
}

async fn prepare(
    source: Arc<WorkerSource>,
    config: BenchmarkLoadConfig,
    engine: Engine,
    control: &Control,
    cleanup: &mut Vec<tokio::sync::watch::Receiver<Cleanup>>,
    environments: &mut [Option<BenchmarkEnvironment>],
) -> ApiResult<Vec<Node>> {
    let quotas = source.reserve_connections(config)?;
    let mut opening = Vec::new();
    for (index, quota) in quotas.into_iter().enumerate() {
        let started = PrivateWorker::start_open(source.clone(), quota, control.clone())?;
        cleanup.push(started.cleanup);
        opening.push((index, started.result));
    }
    let mut setup = Vec::new();
    for (index, opened) in opening {
        let worker = observe(opened, control).await?;
        setup.push((index, worker.start_setup(engine, control.clone())?));
    }
    let mut nodes = Vec::new();
    for (index, ready) in setup {
        let ready = observe(ready, control).await?;
        environments[index] = Some(ready.environment);
        nodes.push(Node {
            index: index as u16,
            worker: ready.worker,
        });
    }
    Ok(nodes)
}

async fn phase(
    shared: Arc<Shared>,
    nodes: Vec<Node>,
    warmup: bool,
    limits: &[sift_protocol::BenchmarkLimits],
) -> Vec<Node> {
    let schedule = Arc::new(
        sift_core::performance::load::worker_phase(limits, shared.config, warmup)
            .expect("validated load schedule"),
    );
    let mut tasks = tokio::task::JoinSet::new();
    for node in nodes {
        let shared = shared.clone();
        let schedule = schedule.clone();
        tasks.spawn(async move {
            for (position, &case) in schedule.iter().enumerate() {
                let (record, input) = shared.admit(node.index, case, warmup).await?;
                let control = input.control.clone();
                let result = match node.worker.clone().start_attempt(input) {
                    Ok(receive) => observe(receive, &control).await,
                    Err(error) => Err(error),
                };
                if !shared.record(record, warmup, result).await {
                    return None;
                }
                let delay = shared.requests[case as usize].delay_ms;
                if delay > 0 && position + 1 < schedule.len() {
                    tokio::select! {
                        _ = shared.control.cancellation.cancelled() => return None,
                        _ = tokio::time::sleep_until(shared.control.deadline) => return None,
                        _ = tokio::time::sleep(Duration::from_millis(delay)) => {},
                    }
                }
            }
            Some(node)
        });
    }
    let mut finished = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Some(node)) => finished.push(node),
            Ok(None) => {}
            Err(_) => shared.stop("Stopped: load controller failed; native owners retain cleanup"),
        }
    }
    finished
}

async fn cleaned(receipts: &mut [tokio::sync::watch::Receiver<Cleanup>]) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut complete = true;
    for receipt in receipts {
        loop {
            let state = *receipt.borrow_and_update();
            if state != Cleanup::Pending {
                complete &= state == Cleanup::Closed;
                break;
            }
            if !matches!(
                tokio::time::timeout_at(deadline, receipt.changed()).await,
                Ok(Ok(()))
            ) {
                complete = false;
                break;
            }
        }
    }
    complete
}

pub(super) async fn run(
    source: Arc<WorkerSource>,
    request: sift_protocol::BenchmarkSuiteRequest,
    requests: Vec<BenchmarkRequest>,
    identities: Vec<(String, String)>,
    cancellation: CancellationToken,
) -> ApiResult<sift_protocol::BenchmarkSuiteReport> {
    let config = request.order.load_config().expect("concurrent route");
    let limits = sift_core::performance_suite::effective_limits(&request);
    sift_core::performance::load::planned_attempts(&limits, config)
        .map_err(|message| ApiError::BadRequest(message.into()))?;
    let engine = source
        .entry
        .driver
        .semantic_engine()
        .expect("captured SQL dialect");
    let deadline = source
        .store
        .inner
        .benchmarks
        .get(&(source.session, source.connection))
        .map(|run| run.deadline)
        .unwrap_or_else(tokio::time::Instant::now);
    let control = Control {
        cancellation,
        deadline,
    };
    let shared = Arc::new(Shared {
        source,
        requests: Arc::new(requests),
        control: control.clone(),
        config,
        origin: tokio::time::Instant::now(),
        ledger: tokio::sync::Mutex::new(Ledger {
            next_start: tokio::time::Instant::now(),
            case_deadlines: vec![None; limits.len()],
            counts: vec![0; limits.len()],
            records: Vec::new(),
        }),
        stopped: Mutex::new(None),
    });
    // Bounded bookkeeping/expanded workload reservation, held through observation.
    let _bookkeeping = shared.source.reserve_bytes(4 * 1024 * 1024)?;
    let mut environments = vec![None; config.workers as usize];
    let mut receipts = Vec::new();
    let mut warnings = vec![
        "Client elapsed/first row/full consumption exclude desktop network/rendering; native database execution time is unavailable".into(),
        "Private reused worker connections use profile defaults; editor session overrides/transactions are not replayed. Cache, data and database load equivalence are unknown".into(),
        "Global rate limits client attempt admission, not native wire dispatch. Throughput and occupancy describe client observations, not database CPU/IO/saturation or outstanding native work after timeout".into(),
        "Only identical frozen cases are pooled across workers; different cases and instrumented Profile clocks are separate. Dependent concurrent samples withhold IID uncertainty".into(),
        "Read-only classification is not a sandbox for external function effects; SQL Server requires a read-only native account".into(),
    ];
    let mut measured_window = None;
    match prepare(
        shared.source.clone(),
        config,
        engine,
        &control,
        &mut receipts,
        &mut environments,
    )
    .await
    {
        Ok(nodes) => {
            let nodes = phase(shared.clone(), nodes, true, &limits).await;
            if nodes.len() == config.workers as usize
                && control.check().is_ok()
                && shared.source.check_all().is_ok()
            {
                let start_ns = nanos(shared.origin.elapsed());
                let mut ledger = shared.ledger.lock().await;
                ledger.next_start = ledger.next_start.max(tokio::time::Instant::now());
                drop(ledger);
                let nodes = phase(shared.clone(), nodes, false, &limits).await;
                measured_window = Some(BenchmarkLoadWindow {
                    start_ns,
                    end_ns: nanos(shared.origin.elapsed()),
                });
                drop(nodes);
            } else {
                shared.stop("Stopped before measurement: warmups, source authority or sampling budget did not permit the barrier");
            }
        }
        Err(_) => {
            shared.stop("Setup stopped or was refused; workload sampling did not start");
        }
    }
    shared.source.store.update_performance_progress(
        shared.source.session,
        shared.source.connection,
        request.run_id,
        PerformanceRunKind::Benchmark,
        PerformanceStage::CleaningUp,
        None,
    );
    let cleanup_complete = cleaned(&mut receipts).await;
    if !cleanup_complete {
        warnings.push("Native cleanup is pending, failed or unobserved; private owners retain quota and shutdown ownership beyond observation".into());
    }
    if let Some(reason) = *shared.stopped.lock().unwrap() {
        warnings.push(reason.into());
    }
    let mut ledger = shared.ledger.lock().await;
    let actual_order = ledger
        .records
        .iter()
        .map(|record| record.case)
        .collect::<Vec<_>>();
    let mut samples = vec![Vec::new(); limits.len()];
    let mut traces = vec![Vec::new(); limits.len()];
    for record in &mut ledger.records {
        if let Some((sample, trace)) = record.sample.take() {
            samples[record.case as usize].push(sample);
            traces[record.case as usize].push(trace);
        } else {
            // Missing observer settlement is an internal error, never fabricated
            // success or a silently shortened schedule.
            return Err(ApiError::Internal(
                "Load controller did not settle an admitted observation".into(),
            ));
        }
    }
    let completed = cleanup_complete
        && measured_window.is_some()
        && actual_order.len()
            == sift_core::performance::load::planned_attempts(&limits, config)
                .expect("validated load")
        && samples
            .iter()
            .flatten()
            .all(|sample| sample.outcome == BenchmarkOutcome::Success);
    let mut cases = Vec::new();
    for (index, ((original, (query_id, case_id)), samples)) in shared
        .requests
        .iter()
        .zip(identities)
        .zip(samples)
        .enumerate()
    {
        let mut aggregate = original.clone();
        aggregate.warmups *= u32::from(config.workers);
        aggregate.iterations *= u32::from(config.workers);
        let environment = environments
            .iter()
            .flatten()
            .next()
            .cloned()
            .unwrap_or_else(|| benchmark_environment(engine));
        let mut report = report(&aggregate, engine, samples, warnings.clone(), environment);
        report.suite_context = Some(Box::new(sift_protocol::BenchmarkSuiteContext {
            version: 4,
            order: Some(request.order),
            run_id: request.run_id,
            query_id,
            case_id,
            case_index: index as u16,
            case_count: limits.len() as u16,
            actual_order: actual_order.clone(),
            suite_completed: completed,
            case_limits: limits.clone(),
            shared_total_budget_ms: Some(request.limits.total_budget_ms),
            load: Some(Box::new(BenchmarkLoadContext {
                config,
                worker_environments: environments.clone(),
                measured_window,
                traces: std::mem::take(&mut traces[index]),
            })),
        }));
        sift_core::performance_suite::validate_context(&report)
            .map_err(|_| ApiError::Internal("Load report lineage is inconsistent".into()))?;
        cases.push(report);
    }
    let observed = cases
        .iter()
        .enumerate()
        .map(|(index, report)| sift_core::performance::load::LoadCase {
            index: index as u16,
            samples: &report.samples,
            context: report
                .suite_context
                .as_ref()
                .unwrap()
                .load
                .as_ref()
                .unwrap(),
        })
        .collect::<Vec<_>>();
    let summary =
        sift_core::performance::load::summarize_suite(&limits, &actual_order, &observed, completed)
            .map_err(|_| ApiError::Internal("Load suite accounting is inconsistent".into()))?;
    Ok(sift_protocol::BenchmarkSuiteReport {
        run_id: request.run_id,
        cases,
        completed,
        load_summary: Some(summary),
    })
}
