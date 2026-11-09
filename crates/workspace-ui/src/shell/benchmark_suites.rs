//! Structured Vim suite drafts, frozen reviews and separate case reports.
use super::*;
use sift_protocol::{
    BenchmarkParameterCase, BenchmarkSuiteQuery, BenchmarkSuiteReport, BenchmarkSuiteRequest,
};

struct CaseDraft {
    id: Entity<TextInput>,
    params: Entity<TextInput>,
    limits: Option<[Entity<TextInput>; 5]>,
}
struct QueryDraft {
    id: Entity<TextInput>,
    sql: Entity<TextInput>,
    cases: Vec<CaseDraft>,
}
pub(super) struct State {
    item: u64,
    source: MonitorActionSource,
    identity: PerformanceConnectionIdentity,
    anchor: String,
    queries: Vec<QueryDraft>,
    query: usize,
    case: usize,
    limits: [Entity<TextInput>; 5],
    order: sift_protocol::BenchmarkSuiteOrder,
    seed: Entity<TextInput>,
    workers: Entity<TextInput>,
    starts_per_second: Entity<TextInput>,
    review: Option<BenchmarkSuiteRequest>,
    submitted: Option<BenchmarkSuiteRequest>,
    running: Option<uuid::Uuid>,
    report: Option<BenchmarkSuiteReport>,
    selected: usize,
    error: Option<String>,
}
const CASE_LIMITS: [&str; 5] = [
    "Case warm-ups",
    "Case measured reads",
    "Case query timeout (ms)",
    "Case total budget (ms)",
    "Case delay (ms)",
];
const LIMITS: [&str; 5] = [
    "Warm-ups per case",
    "Measured reads per case",
    "Query timeout (ms)",
    "Shared total budget (ms)",
    "Delay (ms)",
];
fn field(
    value: String,
    label: &str,
    multiline: bool,
    cx: &mut Context<WorkspaceShell>,
) -> Entity<TextInput> {
    cx.new(|cx| {
        let input = TextInput::new(value, label, cx).aria_label(label);
        if multiline {
            input.multiline()
        } else {
            input
        }
    })
}
fn case(id: String, params: String, cx: &mut Context<WorkspaceShell>) -> CaseDraft {
    CaseDraft {
        limits: None,
        id: field(id, "Case ID", false, cx),
        params: field(
            params,
            "Parameters: typed Value array, e.g. [{\"kind\":\"int64\",\"value\":1}]",
            true,
            cx,
        ),
    }
}
fn next_id<'a>(
    prefix: &str,
    inputs: impl Iterator<Item = &'a Entity<TextInput>>,
    cx: &App,
) -> String {
    let used = inputs
        .map(|input| input.read(cx).text().to_owned())
        .collect::<std::collections::HashSet<_>>();
    (1..)
        .map(|index| format!("{prefix}_{index}"))
        .find(|id| !used.contains(id))
        .expect("finite draft has a free ID")
}
fn read_limits(
    inputs: &[Entity<TextInput>; 5],
    cx: &App,
) -> Result<sift_protocol::BenchmarkLimits, String> {
    let numbers = inputs.each_ref().map(|input| {
        input
            .read(cx)
            .text()
            .parse::<u64>()
            .map_err(|_| "Suite limits must be nonnegative integers".to_owned())
    });
    let [warmups, iterations, timeout, budget, delay] = numbers;
    Ok(sift_protocol::BenchmarkLimits {
        warmups: u32::try_from(warmups?).map_err(|_| "Warm-up count is too large")?,
        iterations: u32::try_from(iterations?).map_err(|_| "Measured count is too large")?,
        query_timeout_ms: timeout?,
        total_budget_ms: budget?,
        delay_ms: delay?,
    })
}
fn describe_limits(limits: sift_protocol::BenchmarkLimits) -> String {
    format!(
        "{} warm-ups · {} measured · {} ms timeout · {} ms budget · {} ms delay",
        limits.warmups,
        limits.iterations,
        limits.query_timeout_ms,
        limits.total_budget_ms,
        limits.delay_ms
    )
}
fn order_name(order: sift_protocol::BenchmarkSuiteOrder) -> &'static str {
    match order {
        sift_protocol::BenchmarkSuiteOrder::Serial => "Serial",
        sift_protocol::BenchmarkSuiteOrder::RoundRobin => "Round robin",
        sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { .. } => "Shuffled rounds v1",
        sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 { .. } => "Concurrent load v1",
    }
}
impl State {
    fn inputs(&self) -> Vec<Entity<TextInput>> {
        let q = &self.queries[self.query];
        let c = &q.cases[self.case];
        let mut inputs = vec![q.id.clone(), q.sql.clone(), c.id.clone(), c.params.clone()];
        if let Some(limits) = &c.limits {
            inputs.extend(limits.iter().cloned());
        }
        if matches!(
            self.order,
            sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { .. }
        ) {
            inputs.push(self.seed.clone());
        }
        if self.order.load_config().is_some() {
            inputs.push(self.workers.clone());
            inputs.push(self.starts_per_second.clone());
        }
        inputs.extend(self.limits.iter().cloned());
        inputs
    }
    fn request(&self, run_id: uuid::Uuid, cx: &App) -> Result<BenchmarkSuiteRequest, String> {
        let limits = read_limits(&self.limits, cx)?;
        let queries =
            self.queries
                .iter()
                .map(|q| {
                    if q.sql.read(cx).text().len() > 1024 * 1024 {
                        return Err("Suite SQL exceeds 1 MiB".into());
                    }
                    Ok(BenchmarkSuiteQuery {
                        id: q.id.read(cx).text().to_owned(),
                        sql: q.sql.read(cx).text().to_owned(),
                        cases: q
                            .cases
                            .iter()
                            .map(|c| {
                                if c.params.read(cx).text().len() > 256 * 1024 {
                                    return Err("Case parameter data exceeds 256 KiB".into());
                                }
                                Ok(BenchmarkParameterCase {
                                    limits: c
                                        .limits
                                        .as_ref()
                                        .map(|inputs| read_limits(inputs, cx))
                                        .transpose()?,
                                    id: c.id.read(cx).text().to_owned(),
                                    params: serde_json::from_str(c.params.read(cx).text())
                                        .map_err(|_| {
                                            "Case parameters must be a JSON array of typed values"
                                                .to_owned()
                                        })?,
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
        let request = BenchmarkSuiteRequest {
            order: match self.order {
                sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 { failure_policy, .. } => {
                    sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 {
                        workers: self
                            .workers
                            .read(cx)
                            .text()
                            .parse()
                            .map_err(|_| "Workers must be an integer from 1 to 8")?,
                        max_starts_per_second: self
                            .starts_per_second
                            .read(cx)
                            .text()
                            .parse()
                            .map_err(|_| "Global start rate must be an integer from 1 to 1000")?,
                        failure_policy,
                    }
                }
                sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { .. } => {
                    sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 {
                        seed: self
                            .seed
                            .read(cx)
                            .text()
                            .parse()
                            .map_err(|_| "Seed must be an unsigned 64-bit integer")?,
                    }
                }
                order => order,
            },
            run_id,
            queries,
            limits,
            workload_confirmed: true,
        };
        sift_core::performance_suite::validate(&request).map_err(str::to_owned)?;
        Ok(request)
    }
}
impl WorkspaceShell {
    fn suite_source_current(&self, state: &State, cx: &App) -> bool {
        self.selected_instance_id.as_deref() == Some(state.source.instance_id.as_str())
            && self.selected_tenant_id() == Some(state.source.tenant_id)
            && self.profile_is_connected(state.source.profile_id)
            && self
                .performance_connection_ids
                .get(&state.source.profile_id)
                == Some(&state.identity)
            && self.query_profile_id(state.item, cx) == Some(state.source.profile_id)
    }
    pub(super) fn suite_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.benchmark_suite.as_ref().is_some_and(|state| {
            state
                .inputs()
                .iter()
                .any(|input| input.focus_handle(cx).is_focused(window))
        })
    }
    pub(super) fn open_benchmark_suite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_benchmark_suite_mode(false, window, cx);
    }
    pub(super) fn open_benchmark_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = self
            .benchmark_suite
            .as_ref()
            .map(|state| state.source.request_id);
        self.open_benchmark_suite_mode(true, window, cx);
        if let Some(state) = &mut self.benchmark_suite {
            if Some(state.source.request_id) != before {
                state.order = sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 {
                    workers: 2,
                    max_starts_per_second: 10,
                    failure_policy: sift_protocol::BenchmarkLoadFailurePolicy::StopRun,
                };
                cx.notify();
            }
        }
    }
    fn open_benchmark_suite_mode(
        &mut self,
        fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((item, results)) = self.focused_pane_results_item(cx) else {
            self.show_error_toast("Open a query tab before composing a suite".into(), cx);
            return;
        };
        if let Some(state) = &self.benchmark_suite {
            if !fresh && state.item == item && self.suite_source_current(state, cx) {
                self.modal = Some(Modal::BenchmarkSuite);
                self.focus_handle.focus(window, cx);
                cx.notify();
                return;
            }
            if state.running.is_some() {
                self.show_error_toast(
                    "Return to the original suite tab and connection before reopening it".into(),
                    cx,
                );
                return;
            }
        }
        let Some(profile) = self.query_profile_id(item, cx) else {
            return;
        };
        let Some(identity) = self.performance_connection_ids.get(&profile).copied() else {
            self.show_error_toast("Reconnect before composing a suite".into(), cx);
            return;
        };
        let Some(instance) = self.selected_instance_id.clone() else {
            return;
        };
        let Some(tenant) = self.selected_tenant_id() else {
            return;
        };
        let tab_instance = self
            .database_source(item, cx)
            .map(|source| source.instance_id)
            .or_else(|| {
                self.query_semantic_targets
                    .get(&item)
                    .map(|target| target.instance_id.clone())
            });
        if tab_instance.as_deref() != Some(instance.as_str())
            || !self.profile_is_connected(profile)
            || results.read(cx).performance_busy()
        {
            self.show_error_toast(
                "Use a connected query tab with no active Benchmark or Profile".into(),
                cx,
            );
            return;
        }
        if !self.require_operation(
            sift_protocol::OperationKind::BenchmarkSuite,
            "Compose benchmark suite",
            cx,
        ) {
            return;
        }
        let anchor = self.targeted_query_sql(item, cx);
        let params = match self.remembered_query_params(item, &anchor, cx) {
            Ok(params) => params,
            Err(error) => {
                self.show_error_toast(error, cx);
                return;
            }
        };
        let values = match results.read(cx).benchmark_limits(cx) {
            Ok(limits) => [
                limits.warmups.to_string(),
                limits.iterations.to_string(),
                limits.query_timeout_ms.to_string(),
                limits.total_budget_ms.to_string(),
                limits.delay_ms.to_string(),
            ],
            Err(error) => {
                self.show_error_toast(error, cx);
                return;
            }
        };
        let limits = std::array::from_fn(|i| field(values[i].clone(), LIMITS[i], false, cx));
        self.benchmark_suite = Some(State {
            item,
            source: MonitorActionSource {
                instance_id: instance,
                tenant_id: tenant,
                profile_id: profile,
                session_id: identity.session,
                connection_id: identity.connection,
                request_id: uuid::Uuid::new_v4(),
            },
            identity,
            anchor: anchor.clone(),
            queries: vec![QueryDraft {
                id: field("query_1".into(), "Query ID", false, cx),
                sql: field(anchor, "SQL", true, cx),
                cases: vec![case(
                    "case_1".into(),
                    serde_json::to_string(&params).expect("parameter values serialize"),
                    cx,
                )],
            }],
            query: 0,
            case: 0,
            limits,
            order: sift_protocol::BenchmarkSuiteOrder::Serial,
            seed: field("0".into(), "Shuffle seed (u64)", false, cx),
            workers: field("2".into(), "Private load workers (1–8)", false, cx),
            starts_per_second: field("10".into(), "Global starts per second (1–1000)", false, cx),
            review: None,
            submitted: None,
            running: None,
            report: None,
            selected: 0,
            error: None,
        });
        self.modal = Some(Modal::BenchmarkSuite);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
    fn review_benchmark_suite(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.benchmark_suite else {
            return;
        };
        let result = if !self.suite_source_current(state, cx)
            || self.targeted_query_sql(state.item, cx) != state.anchor
        {
            Err("Editor or connection changed; open a fresh suite".into())
        } else {
            state.request(uuid::Uuid::new_v4(), cx)
        };
        let state = self.benchmark_suite.as_mut().unwrap();
        match result {
            Ok(request) => {
                state.review = Some(request);
                state.error = None;
            }
            Err(error) => state.error = Some(error),
        }
        cx.notify();
    }
    fn confirm_benchmark_suite(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.benchmark_suite else {
            return;
        };
        let Some(review) = &state.review else {
            return;
        };
        let valid = self.suite_source_current(state, cx)
            && self.targeted_query_sql(state.item, cx) == state.anchor
            && state
                .request(review.run_id, cx)
                .ok()
                .and_then(|request| serde_json::to_value(request).ok())
                == serde_json::to_value(review).ok();
        if !valid {
            let state = self.benchmark_suite.as_mut().unwrap();
            state.review = None;
            state.error = Some("Draft, editor or connection changed; review a fresh suite".into());
            cx.notify();
            return;
        }
        let item = state.item;
        let identity = state.identity;
        let mut source = state.source.clone();
        let request = review.clone();
        let run_id = request.run_id;
        source.request_id = run_id;
        if !self.require_operation(
            sift_protocol::OperationKind::BenchmarkSuite,
            "Run benchmark suite",
            cx,
        ) {
            return;
        }
        let Some(results) = self
            .panes
            .iter()
            .find_map(|pane| pane.read(cx).results.get(&item).cloned())
        else {
            return;
        };
        if results.read(cx).performance_busy() {
            return;
        }
        results.update(cx, |view, cx| view.begin_benchmark_suite(run_id, cx));
        let sent = self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::BenchmarkSuite {
                    item_id: item,
                    source: source.clone(),
                    request: Box::new(request.clone()),
                })
                .is_ok()
        });
        if !sent {
            results.update(cx, |view, cx| {
                view.set_benchmark_result(run_id, Err("Database executor unavailable".into()), cx)
            });
            return;
        }
        self.running_benchmarks
            .insert(item, (run_id, source.profile_id));
        self.performance_progress_sources.insert(
            run_id,
            PerformanceProgressSource {
                instance_id: source.instance_id.clone(),
                profile_id: source.profile_id,
                identity,
            },
        );
        let state = self.benchmark_suite.as_mut().unwrap();
        state.source = source;
        state.review = None;
        state.submitted = Some(request);
        state.running = Some(run_id);
        state.error = None;
        cx.notify();
    }
    pub(super) fn finish_benchmark_suite(
        &mut self,
        item: u64,
        run: uuid::Uuid,
        response: Result<Box<BenchmarkSuiteReport>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.benchmark_suite else {
            return;
        };
        if state.item != item || state.running != Some(run) {
            return;
        }
        let current = self.suite_source_current(state, cx);
        let response = response.and_then(|report| {
            validate_reply(
                state
                    .submitted
                    .as_ref()
                    .expect("active suite has a request"),
                &report,
            )
            .map(|()| *report)
        });
        self.performance_progress_sources.remove(&run);
        if self
            .running_benchmarks
            .get(&item)
            .is_some_and(|(id, _)| *id == run)
        {
            self.running_benchmarks.remove(&item);
        }
        let state = self.benchmark_suite.as_mut().unwrap();
        state.running = None;
        state.submitted = None;
        let result = if current {
            response
        } else {
            Err("Suite connection changed; reopen on the original source".into())
        };
        match result {
            Ok(report) => {
                state.report = Some(report);
                state.selected = 0;
                state.error = None;
            }
            Err(error) => state.error = Some(error),
        }
        let selected = state
            .report
            .as_ref()
            .and_then(|report| report.cases.first())
            .cloned();
        let error = state.error.clone();
        for pane in &self.panes {
            if let Some(results) = pane.read(cx).results.get(&item).cloned() {
                results.update(cx, |view, cx| {
                    view.set_benchmark_result(
                        run,
                        selected.clone().ok_or_else(|| {
                            error
                                .clone()
                                .unwrap_or_else(|| "Suite returned no cases".into())
                        }),
                        cx,
                    );
                    if let Some(report) = selected {
                        view.show_suite_case(report, cx);
                    }
                });
                break;
            }
        }
        cx.notify();
    }
    fn open_suite_case(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = &self.benchmark_suite else {
            return;
        };
        if !self.suite_source_current(state, cx) {
            return;
        }
        let Some(report) = state
            .report
            .as_ref()
            .and_then(|report| report.cases.get(state.selected))
            .cloned()
        else {
            return;
        };
        let item = state.item;
        if save {
            self.open_benchmark_library_candidate(report, None, None, cx);
            return;
        }
        for pane in &self.panes {
            if let Some(results) = pane.read(cx).results.get(&item).cloned() {
                results.update(cx, |view, cx| view.show_suite_case(report, cx));
                break;
            }
        }
        self.modal = None;
        self.focus_results(window, cx);
        cx.notify();
    }
    pub(super) fn benchmark_suite_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::BenchmarkSuite) {
            return;
        }
        let Some(state) = &self.benchmark_suite else {
            return;
        };
        let inputs = state.inputs();
        if let Some(index) = inputs
            .iter()
            .position(|input| input.focus_handle(cx).is_focused(window))
        {
            match event.keystroke.key.as_str() {
                "escape" => self.focus_handle.focus(window, cx),
                "tab" => {
                    let next = if event.keystroke.modifiers.shift {
                        (index + inputs.len() - 1) % inputs.len()
                    } else {
                        (index + 1) % inputs.len()
                    };
                    inputs[next].focus_handle(cx).focus(window, cx);
                }
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        let key = event.keystroke.key.as_str();
        if event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
            || event.keystroke.modifiers.function
        {
            return;
        }
        if key == "escape" {
            self.modal = None;
            self.restore_active_surface(window, cx);
            cx.stop_propagation();
            return;
        }
        if !self.suite_source_current(state, cx) {
            return;
        }
        if let Some(run_id) = state.running {
            if key == "z" {
                let profile_id = state.source.profile_id;
                if let Some(sender) = &self.executor_sender {
                    let _ = sender.send(ExecutorCommand::CancelBenchmark { profile_id, run_id });
                }
            } else {
                return;
            }
        } else if state.report.is_some() {
            match key {
                "j" | "k" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    let len = state.report.as_ref().unwrap().cases.len();
                    state.selected = if key == "j" {
                        (state.selected + 1) % len
                    } else {
                        (state.selected + len - 1) % len
                    };
                }
                "o" => self.open_suite_case(false, window, cx),
                "s" => self.open_suite_case(true, window, cx),
                "n" => self.open_benchmark_suite_mode(true, window, cx),
                _ => return,
            }
        } else if state.review.is_some() {
            match key {
                "x" => self.confirm_benchmark_suite(cx),
                "e" => self.benchmark_suite.as_mut().unwrap().review = None,
                _ => return,
            }
        } else {
            match key {
                "p" => self.review_benchmark_suite(cx),
                "b" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    state.order = if state.order.load_config().is_some() {
                        sift_protocol::BenchmarkSuiteOrder::Serial
                    } else {
                        sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 {
                            workers: 2,
                            max_starts_per_second: 10,
                            failure_policy: sift_protocol::BenchmarkLoadFailurePolicy::StopRun,
                        }
                    };
                }
                "f" if state.order.load_config().is_some() => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    if let sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 {
                        failure_policy, ..
                    } = &mut state.order
                    {
                        *failure_policy = match failure_policy {
                            sift_protocol::BenchmarkLoadFailurePolicy::StopRun => {
                                sift_protocol::BenchmarkLoadFailurePolicy::RetireWorker
                            }
                            sift_protocol::BenchmarkLoadFailurePolicy::RetireWorker => {
                                sift_protocol::BenchmarkLoadFailurePolicy::StopRun
                            }
                        };
                    }
                }
                "i" | "tab" => inputs[0].focus_handle(cx).focus(window, cx),
                "e" => inputs[1].focus_handle(cx).focus(window, cx),
                "v" => inputs[3].focus_handle(cx).focus(window, cx),
                "r" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    state.order = match state.order {
                        sift_protocol::BenchmarkSuiteOrder::Serial => {
                            sift_protocol::BenchmarkSuiteOrder::RoundRobin
                        }
                        sift_protocol::BenchmarkSuiteOrder::RoundRobin => {
                            sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { seed: 0 }
                        }
                        sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { .. } => {
                            sift_protocol::BenchmarkSuiteOrder::Serial
                        }
                        sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 { .. } => {
                            sift_protocol::BenchmarkSuiteOrder::Serial
                        }
                    };
                }
                "u" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    let c = &mut state.queries[state.query].cases[state.case];
                    c.limits = if c.limits.is_some() {
                        None
                    } else {
                        Some(std::array::from_fn(|index| {
                            field(
                                state.limits[index].read(cx).text().to_owned(),
                                CASE_LIMITS[index],
                                false,
                                cx,
                            )
                        }))
                    };
                }
                "h" | "l" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    let len = state.queries.len();
                    state.query = if key == "l" {
                        (state.query + 1) % len
                    } else {
                        (state.query + len - 1) % len
                    };
                    state.case = 0;
                }
                "j" | "k" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    let len = state.queries[state.query].cases.len();
                    state.case = if key == "j" {
                        (state.case + 1) % len
                    } else {
                        (state.case + len - 1) % len
                    };
                }
                "a" => {
                    let total: usize = state.queries.iter().map(|q| q.cases.len()).sum();
                    if total >= 32 {
                        return;
                    }
                    let shifted = event.keystroke.modifiers.shift;
                    let state = self.benchmark_suite.as_mut().unwrap();
                    if shifted {
                        let id = next_id("query", state.queries.iter().map(|query| &query.id), cx);
                        state.queries.push(QueryDraft {
                            id: field(id, "Query ID", false, cx),
                            sql: field(state.anchor.clone(), "SQL", true, cx),
                            cases: vec![case("case_1".into(), "[]".into(), cx)],
                        });
                        state.query = state.queries.len() - 1;
                        state.case = 0;
                    } else {
                        let q = &mut state.queries[state.query];
                        let id = next_id("case", q.cases.iter().map(|case| &case.id), cx);
                        q.cases.push(case(id, "[]".into(), cx));
                        state.case = q.cases.len() - 1;
                    }
                }
                "d" => {
                    let state = self.benchmark_suite.as_mut().unwrap();
                    if event.keystroke.modifiers.shift {
                        if state.queries.len() > 1 {
                            state.queries.remove(state.query);
                            state.query = state.query.min(state.queries.len() - 1);
                            state.case = 0;
                        }
                    } else {
                        let q = &mut state.queries[state.query];
                        if q.cases.len() > 1 {
                            q.cases.remove(state.case);
                            state.case = state.case.min(q.cases.len() - 1);
                        }
                    }
                }
                _ => return,
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn render_benchmark_suite(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &self.benchmark_suite else {
            return div().child("Suite unavailable").into_any_element();
        };
        let base = div()
            .id("benchmark-suite-composer")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(720.))
            .overflow_y_scroll();
        if !self.suite_source_current(state, cx) {
            return base.child("Original suite connection changed. Esc closes; reconnect and open a fresh suite.").into_any_element();
        }
        let base = base
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Benchmark suite"),
            )
            .child(format!(
                "Profile {} · {} · shared budget",
                state.source.profile_id,
                order_name(state.order)
            ))
            .children(state.error.clone().map(|error| div().child(error)));
        if let Some(report) = &state.report {
            return base.child(format!("Recorded dispatch: {}",sift_core::performance_suite::describe_order(report.cases[0].suite_context.as_ref().and_then(|context|context.order).unwrap_or_default()))).child(if report.completed {"Suite complete"}else{"Suite incomplete; numeric comparisons unavailable"})
                .children(report.load_summary.as_ref().map(|summary| div().whitespace_normal().child(format!(
                    "Measured: {} successful · {} failed · {} timed out · {} cancelled · {} unattempted. Success throughput: {} /s · client peak: {} · client busy: {}. Database CPU/IO/saturation unavailable; outstanding native cleanup is outside these observation clocks.",
                    summary.measured.successful, summary.measured.failed, summary.measured.timed_out, summary.measured.cancelled, summary.measured.unattempted,
                    summary.successes_per_second_milli.map_or_else(|| "unavailable".into(), |rate|format!("{}.{:03}",rate/1000,rate%1000)), summary.client_peak_concurrency,
                    summary.client_busy_permille.map_or_else(|| "unavailable".into(), |busy|format!("{}.{:01}%",busy/10,busy%10)),
                ))))
                .child("j/k cases · o Open selected case in Performance · s Save selected case · n New suite · Esc close")
                .children(report.cases.iter().enumerate().map(|(i,case)| {let context=case.suite_context.as_ref().expect("validated suite case");div().child(format!("{} {}/{} · {} attempts · {}",if i==state.selected {">"}else{" "},context.query_id,context.case_id,case.samples.len(),if case.completed {"complete"}else{"partial/unattempted"}))})).into_any_element();
        }
        if let Some(run) = state.running {
            return base.child(format!("Suite {run} running. Progress and Esc cancellation are available in Performance."))
            .child("z Request cancellation · Esc close and keep running · reopen this composer for all case reports").into_any_element();
        }
        if let Some(review) = &state.review {
            let count: usize = review.queries.iter().map(|query| query.cases.len()).sum();
            let limits = sift_core::performance_suite::effective_limits(review);
            let (planned, ordering) = if let Some(config) = review.order.load_config() {
                (sift_core::performance::load::planned_attempts(&limits, config).expect("validated load review"),
                    format!("{} extra private connections · ≤{} client starts/s across every worker/case · {:?}. Each worker replicates the full cases; successful warmups finish before measurement. Exact concurrent admission order is observed.",config.workers,config.max_starts_per_second,config.failure_policy))
            } else {
                let schedule =
                    sift_core::performance_suite::schedule_ordered(&limits, review.order)
                        .expect("validated suite review");
                (
                    schedule.len(),
                    format!(
                        "Planned dispatch indices (zero-based case list order): {}",
                        schedule
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                )
            };
            return base.child("Frozen review · x Confirm entire workload · e Return to draft · Esc close without running")
                .child(format!("{} · case budgets count elapsed time from first dispatch, including other cases", sift_core::performance_suite::describe_order(review.order)))
                .child(div().whitespace_normal().child(ordering))
                .child(format!("{} queries · {count} cases · {planned} planned workload reads (including every worker; setup reads are additional)",review.queries.len()))
                .child(format!("Defaults: {} warm-ups and {} measured reads · {} ms query timeout · {} ms shared total budget · {} ms delay. Effective limits are listed for each case below.",review.limits.warmups,review.limits.iterations,review.limits.query_timeout_ms,review.limits.total_budget_ms,review.limits.delay_ms))
                .child("Repeated reads can load the database or invoke external effects. Native read-only transactions are used where supported; SQL Server requires a read-only login. Earlier cases can affect later cases.")
                .children(review.queries.iter().map(|q|div().flex().flex_col().gap_1().child(format!("Query {}",q.id)).child(div().font_family("monospace").whitespace_normal().child(q.sql.clone())).children(q.cases.iter().map(|c|div().whitespace_normal().child(format!("{} · {} · {}{}",c.id,serde_json::to_string(&c.params).expect("values serialize"),if c.limits.is_some() {"override · "} else {"inherited · "},describe_limits(c.limits.unwrap_or(review.limits)))))))).into_any_element();
        }
        let q = &state.queries[state.query];
        let c = &q.cases[state.case];
        base.child("h/l queries · j/k cases · a Add case · Shift-A Add query · d/Shift-D Remove case/query · p Review")
            .child("i Query ID · e SQL · v Parameters · u Toggle case limits · r Cycle serial ordering · b Load mode · f Load failure policy · Tab/Shift-Tab fields · Esc returns to commands")
            .child(format!("Query {}/{} · case {}/{}",state.query+1,state.queries.len(),state.case+1,q.cases.len()))
            .child("Query ID").child(q.id.clone()).child("SQL").child(q.sql.clone()).child("Case ID").child(c.id.clone()).child("Typed parameters").child(c.params.clone())
            .child(if c.limits.is_some() { "Case limits override suite defaults; budget also obeys the shared deadline. Exhaustion stops the suite." } else { "Case inherits suite defaults. u enables an independent override." })
            .children(c.limits.iter().flat_map(|limits| CASE_LIMITS.into_iter().zip(limits)).map(|(label,input)|div().flex().flex_col().gap_1().child(label).child(input.clone())))
            .children(matches!(state.order, sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { .. }).then(|| div().flex().flex_col().child("Shuffle seed (u64)").child(state.seed.clone())))
            .children(state.order.load_config().map(|config| div().flex().flex_col()
                .child("Private workers (1–8); each replicates every case's warmup/iteration counts").child(state.workers.clone())
                .child("Global client starts per second (1–1000)").child(state.starts_per_second.clone())
                .child(format!("Failure policy: {:?} · f toggles stop run / retire failed worker. No retry or replacement; any warmup failure stops the run.",config.failure_policy))))
            .child("A case budget includes time spent on other cases. Reaching it while that case is running or queued stops the suite. Shuffling does not establish cache/load equivalence or IID samples.")
            .child("Suite defaults and shared total budget")
            .children(LIMITS.into_iter().zip(&state.limits).map(|(label,input)|div().flex().flex_col().gap_1().child(label).child(input.clone())))
            .into_any_element()
    }
}
fn validate_reply(
    request: &BenchmarkSuiteRequest,
    report: &BenchmarkSuiteReport,
) -> Result<(), String> {
    let expected = request
        .queries
        .iter()
        .flat_map(|q| q.cases.iter().map(move |c| (q, c)))
        .collect::<Vec<_>>();
    let config = request.order.load_config();
    let workers = config.map_or(1, |config| u32::from(config.workers));
    if report.run_id != request.run_id
        || report.cases.len() != expected.len()
        || (report.completed && !report.cases.iter().all(|r| r.completed))
        || (config.is_none() && report.completed != report.cases.iter().all(|r| r.completed))
    {
        return Err("Suite response identity or case count differs from the request".into());
    }
    let limits = sift_core::performance_suite::effective_limits(request);
    let mut ids = std::collections::HashSet::new();
    for (index, (case, (query, params))) in report.cases.iter().zip(expected).enumerate() {
        sift_core::performance_suite::validate_context(case).map_err(str::to_owned)?;
        let c = case
            .suite_context
            .as_ref()
            .ok_or("Suite case context is missing")?;
        if c.run_id != request.run_id
            || c.version != if config.is_some() { 4 } else { 3 }
            || c.order != Some(request.order)
            || c.case_limits != limits
            || c.shared_total_budget_ms != Some(request.limits.total_budget_ms)
            || c.query_id != query.id
            || c.case_id != params.id
            || usize::from(c.case_index) != index
            || usize::from(c.case_count) != report.cases.len()
            || c.suite_completed != report.completed
            || case.sql != query.sql
            || case.parameter_count != params.params.len()
            || !ids.insert(case.run_id)
            || case.warmups != limits[index].warmups * workers
            || case.requested_iterations != limits[index].iterations * workers
            || case.query_timeout_ms != limits[index].query_timeout_ms
            || case.total_budget_ms != limits[index].total_budget_ms
            || case.delay_ms != limits[index].delay_ms
            || c.actual_order
                != report.cases[0]
                    .suite_context
                    .as_ref()
                    .ok_or("Suite context is missing")?
                    .actual_order
        {
            return Err("Suite case response differs from the frozen workload".into());
        }
    }
    if config.is_some() {
        let observed = report
            .cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                Ok(sift_core::performance::load::LoadCase {
                    index: index as u16,
                    samples: &case.samples,
                    context: case
                        .suite_context
                        .as_ref()
                        .and_then(|context| context.load.as_deref())
                        .ok_or("Concurrent observations are missing")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let order = &report.cases[0].suite_context.as_ref().unwrap().actual_order;
        let summary = sift_core::performance::load::summarize_suite(
            &limits,
            order,
            &observed,
            report.completed,
        )
        .map_err(str::to_owned)?;
        if report.load_summary.as_ref() != Some(&summary) {
            return Err("Concurrent summary differs from its raw observations".into());
        }
    } else if report.load_summary.is_some() {
        return Err("Serial suite unexpectedly returned concurrent metrics".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    fn reply(request: &BenchmarkSuiteRequest) -> BenchmarkSuiteReport {
        let expected = request
            .queries
            .iter()
            .flat_map(|q| q.cases.iter().map(move |c| (q, c)))
            .collect::<Vec<_>>();
        let limits = sift_core::performance_suite::effective_limits(request);
        let order = sift_core::performance_suite::schedule_ordered(&limits, request.order).unwrap();
        let cases=expected.iter().enumerate().map(|(i,(q,c))| {
            let mut report:sift_protocol::BenchmarkReport=serde_json::from_value(serde_json::json!({
                "version":2,"run_id":if i==0 {request.run_id}else{uuid::Uuid::new_v4()},"engine":"postgres","sql":q.sql,"captured_at":"2026-11-06T00:00:00Z",
                "warmups":limits[i].warmups,"requested_iterations":limits[i].iterations,"query_timeout_ms":limits[i].query_timeout_ms,"total_budget_ms":limits[i].total_budget_ms,"delay_ms":limits[i].delay_ms,"parameter_count":c.params.len(),"completed":true,"warnings":[],
                "samples":(0..limits[i].warmups+limits[i].iterations).map(|ordinal|serde_json::json!({"ordinal":ordinal,"warmup":ordinal<limits[i].warmups,"outcome":"success","elapsed_ns":100,"rows":1})).collect::<Vec<_>>(),"median_ns":100.0
            })).unwrap();
            report.suite_context=Some(Box::new(sift_protocol::BenchmarkSuiteContext {
load: None,
 order: Some(request.order),
 case_limits: limits.clone(), shared_total_budget_ms: Some(request.limits.total_budget_ms),version:3,run_id:request.run_id,query_id:q.id.clone(),case_id:c.id.clone(),case_index:i as u16,case_count:expected.len() as u16,actual_order:order.clone(),suite_completed:true}));report
        }).collect();
        BenchmarkSuiteReport {
            load_summary: None,
            run_id: request.run_id,
            cases,
            completed: true,
        }
    }
    #[gpui::test]
    fn vim_suite_freezes_cases_refuses_changed_sources_and_opens_separate_reports(
        cx: &mut TestAppContext,
    ) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(32);
        let identity = PerformanceConnectionIdentity {
            session: sift_protocol::SessionId(21),
            connection: sift_protocol::ConnectionId(22),
            generation: uuid::Uuid::new_v4(),
        };
        workspace.update(&mut cx, |shell, _| {
            shell.executor_sender = Some(sender);
            shell.selected_instance_id = Some("local".into());
            shell.connected_profiles.insert(2);
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 2,
                name: "suite".into(),
            };
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Local".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.performance_connection_ids.insert(2, identity);
        });
        let (item, editor, results) = workspace.update_in(&mut cx, |shell, window, cx| {
            shell.new_query(window, cx);
            let pane = shell.panes[shell.active_pane].clone();
            let item = pane.read(cx).active_item().unwrap().id;
            let editor = pane.read(cx).editor(item).unwrap();
            editor.update(cx, |view, cx| view.replace_text_from_owner("SELECT 1", cx));
            shell.query_semantic_targets.insert(
                item,
                SemanticConnectionTarget {
                    instance_id: "local".into(),
                    tenant_id: 1,
                    profile_id: 2,
                    profile_name: "suite".into(),
                    provider_id: sift_protocol::Engine::Postgres.provider_id(),
                    database: None,
                },
            );
            let results = pane.read(cx).results.get(&item).unwrap().clone();
            shell.open_benchmark_suite(window, cx);
            (item, editor, results)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("i escape a k d a shift-a");
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.benchmark_suite.as_mut().unwrap();
            assert_eq!(state.queries.len(), 2);
            assert_eq!(state.queries[0].cases.len(), 2);
            assert_eq!(state.queries[0].cases[0].id.read(cx).text(), "case_2");
            assert_eq!(state.queries[0].cases[1].id.read(cx).text(), "case_1");
            state.queries[0]
                .sql
                .update(cx, |input, cx| input.set_text("SELECT $1", cx));
            for (i, c) in state.queries[0].cases.iter().enumerate() {
                c.params.update(cx, |input, cx| {
                    input.set_text(
                        serde_json::to_string(&vec![sift_protocol::Value::Int64(i as i64 + 11)])
                            .unwrap(),
                        cx,
                    )
                });
            }
            for (input, value) in state.limits.iter().zip(["0", "1", "100", "1000", "0"]) {
                input.update(cx, |input, cx| input.set_text(value, cx));
            }
        });
        cx.simulate_keystrokes("b f p");
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.benchmark_suite.as_ref().unwrap();
            assert!(matches!(
                state.review.as_ref().unwrap().order,
                sift_protocol::BenchmarkSuiteOrder::ConcurrentV1 {
                    workers: 2,
                    max_starts_per_second: 10,
                    failure_policy: sift_protocol::BenchmarkLoadFailurePolicy::RetireWorker,
                }
            ));
            state
                .workers
                .update(cx, |input, cx| input.set_text("3", cx));
        });
        cx.simulate_keystrokes("x");
        assert!(
            commands.try_recv().is_err(),
            "hidden load budget edits invalidate frozen review"
        );
        workspace.update(&mut cx, |shell, cx| {
            shell
                .benchmark_suite
                .as_ref()
                .unwrap()
                .workers
                .update(cx, |input, cx| input.set_text("2", cx));
        });
        cx.simulate_keystrokes("b r r");
        workspace.update(&mut cx, |shell, cx| {
            shell
                .benchmark_suite
                .as_ref()
                .unwrap()
                .seed
                .update(cx, |input, cx| input.set_text("42", cx))
        });
        cx.simulate_keystrokes("p");
        workspace.update(&mut cx, |shell, cx| {
            shell
                .benchmark_suite
                .as_ref()
                .unwrap()
                .seed
                .update(cx, |input, cx| input.set_text("43", cx))
        });
        cx.simulate_keystrokes("x");
        assert!(
            commands.try_recv().is_err(),
            "hidden ordering seed edits invalidate review"
        );
        workspace.update(&mut cx, |shell, cx| {
            shell
                .benchmark_suite
                .as_ref()
                .unwrap()
                .seed
                .update(cx, |input, cx| input.set_text("42", cx))
        });
        cx.simulate_keystrokes("u");
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.benchmark_suite.as_ref().unwrap();
            let limits = state.queries[state.query].cases[state.case]
                .limits
                .as_ref()
                .unwrap();
            for (input, value) in limits.iter().zip(["1", "2", "80", "500", "1"]) {
                input.update(cx, |input, cx| input.set_text(value, cx));
            }
        });
        cx.simulate_keystrokes("p");
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.benchmark_suite.as_ref().unwrap();
            state.queries[state.query].cases[state.case]
                .limits
                .as_ref()
                .unwrap()[1]
                .update(cx, |input, cx| input.set_text("3", cx));
        });
        cx.simulate_keystrokes("x");
        assert!(
            commands.try_recv().is_err(),
            "hidden case-limit edits invalidate review"
        );
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.benchmark_suite.as_ref().unwrap();
            state.queries[state.query].cases[state.case]
                .limits
                .as_ref()
                .unwrap()[1]
                .update(cx, |input, cx| input.set_text("2", cx));
        });
        cx.simulate_keystrokes("p enter");
        assert!(
            commands.try_recv().is_err(),
            "review and Enter never execute"
        );
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.benchmark_suite.as_ref().unwrap();
            assert!(state.review.is_some());
            state.queries[0].cases[0]
                .params
                .update(cx, |input, cx| input.set_text("[]", cx));
        });
        cx.simulate_keystrokes("x");
        assert!(commands.try_recv().is_err());
        workspace.read_with(&cx, |shell, _| {
            assert!(shell.benchmark_suite.as_ref().unwrap().review.is_none())
        });
        workspace.update(&mut cx, |shell, cx| {
            shell.benchmark_suite.as_ref().unwrap().queries[0].cases[0]
                .params
                .update(cx, |input, cx| {
                    input.set_text(
                        serde_json::to_string(&vec![sift_protocol::Value::Int64(11)]).unwrap(),
                        cx,
                    )
                });
        });
        cx.simulate_keystrokes("p");
        editor.update(&mut cx, |view, cx| {
            view.replace_text_from_owner("SELECT 99", cx)
        });
        cx.simulate_keystrokes("x");
        assert!(commands.try_recv().is_err());
        editor.update(&mut cx, |view, cx| {
            view.replace_text_from_owner("SELECT 1", cx)
        });
        cx.simulate_keystrokes("p");
        workspace.update(&mut cx, |shell, cx| {
            shell
                .performance_connection_ids
                .get_mut(&2)
                .unwrap()
                .generation = uuid::Uuid::new_v4();
            shell.confirm_benchmark_suite(cx);
        });
        assert!(commands.try_recv().is_err());
        workspace.update(&mut cx, |shell, _| {
            shell.performance_connection_ids.insert(2, identity);
        });
        cx.simulate_keystrokes("p x");
        let request = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkSuite {
                item_id,
                source,
                request,
            } => {
                assert_eq!(item_id, item);
                assert_eq!(
                    request.order,
                    sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 { seed: 42 }
                );
                let overrides = request
                    .queries
                    .iter()
                    .flat_map(|query| &query.cases)
                    .filter_map(|case| case.limits)
                    .collect::<Vec<_>>();
                assert_eq!(overrides.len(), 1);
                assert_eq!(overrides[0].iterations, 2);
                assert_eq!(overrides[0].total_budget_ms, 500);
                assert_eq!(source.session_id, identity.session);
                assert_eq!(source.connection_id, identity.connection);
                assert_eq!(source.instance_id, "local");
                assert_eq!(source.tenant_id, 1);
                assert_eq!(source.request_id, request.run_id);
                *request
            }
            _ => panic!("expected suite request"),
        };
        assert_eq!(request.queries[0].sql, "SELECT $1");
        assert_eq!(
            request.queries[0].cases[0].params,
            vec![sift_protocol::Value::Int64(11)]
        );
        assert_eq!(
            request.queries[0].cases[1].params,
            vec![sift_protocol::Value::Int64(12)]
        );
        cx.simulate_keystrokes("escape");
        assert!(
            commands.try_recv().is_err(),
            "closing the composer does not cancel execution"
        );
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.open_benchmark_suite(window, cx)
        });
        let (ack, mut received) = tokio::sync::oneshot::channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::PerformanceProgress {
                    item_id: item,
                    instance_id: "local".into(),
                    profile_id: 2,
                    session: identity.session,
                    connection: identity.connection,
                    progress: sift_protocol::PerformanceProgress {
                        run_id: request.run_id,
                        kind: sift_protocol::PerformanceRunKind::Benchmark,
                        stage: sift_protocol::PerformanceStage::Measuring,
                        elapsed_ms: 1,
                        benchmark: Some(sift_protocol::BenchmarkProgressCounters {
                            requested_warmups: 1,
                            requested_iterations: 4,
                            finished_warmups: 0,
                            finished_iterations: 1,
                            last_outcome: Some(sift_protocol::BenchmarkOutcome::Success),
                        }),
                        cancellation_requested: false,
                    },
                    acknowledge: ack,
                },
                cx,
            )
        });
        assert!(received.try_recv().is_ok());
        assert!(results
            .read_with(&cx, |view, _| view.performance_progress_status())
            .is_some());
        cx.simulate_keystrokes("x z");
        assert!(
            matches!(commands.try_recv(),Ok(ExecutorCommand::CancelBenchmark {run_id,..}) if run_id==request.run_id)
        );
        assert!(commands.try_recv().is_err());
        let report = reply(&request);
        validate_reply(&request, &report).unwrap();
        let mut corrupt = report.clone();
        corrupt.cases[0].suite_context.as_mut().unwrap().order =
            Some(sift_protocol::BenchmarkSuiteOrder::RoundRobin);
        assert!(validate_reply(&request, &corrupt).is_err());
        let mut corrupt = report.clone();
        corrupt.cases[1].suite_context.as_mut().unwrap().case_id = "other".into();
        assert!(validate_reply(&request, &corrupt).is_err());
        let mut corrupt = report.clone();
        corrupt.cases[0].suite_context.as_mut().unwrap().case_limits[0].iterations += 1;
        assert!(validate_reply(&request, &corrupt).is_err());
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::BenchmarkSuiteFinished {
                    item_id: item,
                    run_id: uuid::Uuid::new_v4(),
                    response: Ok(Box::new(report.clone())),
                },
                cx,
            );
            assert_eq!(
                shell.benchmark_suite.as_ref().unwrap().running,
                Some(request.run_id)
            );
            shell.on_executor_event(
                ExecutorEvent::BenchmarkSuiteFinished {
                    item_id: item,
                    run_id: request.run_id,
                    response: Ok(Box::new(report.clone())),
                },
                cx,
            );
            assert!(shell.benchmark_suite.as_ref().unwrap().running.is_none());
            assert_eq!(
                shell
                    .benchmark_suite
                    .as_ref()
                    .unwrap()
                    .report
                    .as_ref()
                    .unwrap()
                    .cases
                    .len(),
                3
            );
        });
        let blocked = uuid::Uuid::new_v4();
        results.update(&mut cx, |view, cx| view.begin_benchmark_suite(blocked, cx));
        cx.simulate_keystrokes("n");
        workspace.read_with(&cx, |shell, _| {
            assert_eq!(
                shell
                    .benchmark_suite
                    .as_ref()
                    .unwrap()
                    .report
                    .as_ref()
                    .unwrap()
                    .cases
                    .len(),
                3
            )
        });
        results.update(&mut cx, |view, cx| {
            view.set_benchmark_result(blocked, Err("synthetic concurrent job finished".into()), cx)
        });
        cx.simulate_keystrokes("j o");
        results.read_with(&cx, |view, _| {
            let report = view.benchmark_report().unwrap();
            assert_eq!(report.suite_context.as_ref().unwrap().case_id, "case_1");
            assert!(view.benchmark_source_context().is_none());
        });
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.open_benchmark_suite(window, cx)
        });
        cx.simulate_keystrokes("n p x");
        let next = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkSuite { request, .. } => *request,
            _ => panic!("expected fresh suite"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell
                .performance_connection_ids
                .get_mut(&2)
                .unwrap()
                .generation = uuid::Uuid::new_v4();
            shell.on_executor_event(
                ExecutorEvent::BenchmarkSuiteFinished {
                    item_id: item,
                    run_id: next.run_id,
                    response: Ok(Box::new(reply(&next))),
                },
                cx,
            );
            assert!(shell.benchmark_suite.as_ref().unwrap().report.is_none());
            assert!(shell.benchmark_suite.as_ref().unwrap().error.is_some());
            shell.on_executor_event(
                ExecutorEvent::BenchmarkSuiteFinished {
                    item_id: item,
                    run_id: request.run_id,
                    response: Ok(Box::new(report)),
                },
                cx,
            );
            assert!(shell.benchmark_suite.as_ref().unwrap().report.is_none());
        });
    }
}
