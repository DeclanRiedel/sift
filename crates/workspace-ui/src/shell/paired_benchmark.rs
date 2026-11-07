//! Frozen A/B anchors; no request is sent while pinning or changing order.
use super::*;
#[derive(Clone)]
pub(super) struct Pinned {
    item_id: u64,
    profile_id: i64,
    instance_id: Option<String>,
    tenant_id: Option<i64>,
    identity: PerformanceConnectionIdentity,
    request: sift_protocol::BenchmarkRequest,
    source_context: Option<sift_protocol::BenchmarkSourceContext>,
}
impl WorkspaceShell {
    fn valid_pinned_benchmark(&self, cx: &App) -> bool {
        let Some(pin) = &self.pinned_benchmark else {
            return false;
        };
        pin.instance_id == self.selected_instance_id
            && pin.tenant_id == self.selected_tenant_id()
            && self.profile_is_connected(pin.profile_id)
            && self.performance_connection_ids.get(&pin.profile_id) == Some(&pin.identity)
            && self.query_profile_id(pin.item_id, cx) == Some(pin.profile_id)
            && self.targeted_query_sql(pin.item_id, cx) == pin.request.sql
    }
    pub(super) fn observe_pinned_workload_execution(
        &mut self,
        item_id: u64,
        sql: &str,
        params: &[sift_protocol::Value],
    ) {
        if self.pinned_benchmark.as_ref().is_some_and(|pin| {
            pin.item_id == item_id && (pin.request.sql != sql || pin.request.params != params)
        }) {
            self.pinned_benchmark = None;
        }
    }
    pub(super) fn pin_performance_workload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = &self.pending_performance_review else {
            return;
        };
        let PerformanceReviewRequest::Benchmark(request) = &review.request else {
            return;
        };
        let Some(identity) = self
            .performance_connection_ids
            .get(&review.profile_id)
            .copied()
        else {
            self.show_error_toast("Reconnect before pinning a benchmark workload".into(), cx);
            return;
        };
        self.pinned_benchmark = Some(Pinned {
            item_id: review.item_id,
            profile_id: review.profile_id,
            instance_id: review
                .instance_id
                .clone()
                .or_else(|| self.selected_instance_id.clone()),
            tenant_id: self.selected_tenant_id(),
            identity,
            request: request.clone(),
            source_context: review.source_context.clone(),
        });
        self.cancel_performance_review(cx);
        self.modal = None;
        self.restore_active_surface(window, cx);
        self.show_success_toast("Pinned workload A; no query executed".into(), cx);
        cx.notify();
    }
    pub(super) fn cycle_paired_order(&mut self, cx: &mut Context<Self>) {
        if !self.valid_pinned_benchmark(cx) {
            self.show_error_toast(
                "Pin an unchanged workload A on this connection before pairing".into(),
                cx,
            );
            return;
        }
        let pin = self.pinned_benchmark.as_ref().unwrap();
        let Some(review) = &mut self.pending_performance_review else {
            return;
        };
        if review.profile_id != pin.profile_id
            || review
                .instance_id
                .as_ref()
                .or(self.selected_instance_id.as_ref())
                != pin.instance_id.as_ref()
        {
            self.show_error_toast(
                "Both workloads must use the original instance and connection".into(),
                cx,
            );
            return;
        }
        use sift_protocol::{
            BenchmarkLimits, BenchmarkPairOrder as Order, BenchmarkPairRequest, BenchmarkWorkload,
        };
        review.request = match &review.request {
            PerformanceReviewRequest::Benchmark(request) => {
                PerformanceReviewRequest::Pair(Box::new(BenchmarkPairRequest {
                    verify_results: None,
                    run_id: request.run_id,
                    baseline: BenchmarkWorkload {
                        sql: pin.request.sql.clone(),
                        params: pin.request.params.clone(),
                    },
                    candidate: BenchmarkWorkload {
                        sql: request.sql.clone(),
                        params: request.params.clone(),
                    },
                    limits: BenchmarkLimits {
                        warmups: request.warmups,
                        iterations: request.iterations,
                        query_timeout_ms: request.query_timeout_ms,
                        total_budget_ms: request.total_budget_ms,
                        delay_ms: request.delay_ms,
                    },
                    order: Order::Alternating,
                    workload_confirmed: true,
                }))
            }
            PerformanceReviewRequest::Pair(request) if request.order == Order::Alternating => {
                let mut request = request.clone();
                let seed = request.run_id.as_u128() as u64;
                request.order = Order::RandomizedV1 { seed };
                if self.paired_seed_input.is_none() {
                    self.paired_seed_input =
                        Some(cx.new(|cx| {
                            TextInput::new(seed.to_string(), "Unsigned 64-bit seed", cx)
                        }));
                }
                PerformanceReviewRequest::Pair(request)
            }
            PerformanceReviewRequest::Pair(request) => {
                PerformanceReviewRequest::Benchmark(sift_protocol::BenchmarkRequest {
                    run_id: request.run_id,
                    sql: request.candidate.sql.clone(),
                    params: request.candidate.params.clone(),
                    warmups: request.limits.warmups,
                    iterations: request.limits.iterations,
                    query_timeout_ms: request.limits.query_timeout_ms,
                    total_budget_ms: request.limits.total_budget_ms,
                    delay_ms: request.limits.delay_ms,
                    workload_confirmed: true,
                })
            }
            PerformanceReviewRequest::Profile(_) => return,
        };
        cx.notify();
    }
    pub(super) fn cycle_paired_verification(&mut self, cx: &mut Context<Self>) {
        use sift_protocol::{BenchmarkResultCheck, BenchmarkResultOrder as Order};
        if let Some(PerformanceReviewRequest::Pair(request)) = self
            .pending_performance_review
            .as_mut()
            .map(|review| &mut review.request)
        {
            request.verify_results = match request.verify_results {
                None => Some(BenchmarkResultCheck::default()),
                Some(check) if check.order == Order::Ordered => Some(BenchmarkResultCheck {
                    order: Order::Unordered,
                    ..check
                }),
                _ => None,
            };
            cx.notify();
        }
    }
    pub(super) fn prepare_paired_review(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(PerformanceReviewRequest::Pair(request)) = self
            .pending_performance_review
            .as_ref()
            .map(|review| &review.request)
        else {
            return true;
        };
        let pin_valid = self
            .pinned_benchmark
            .as_ref()
            .is_some_and(|pin| pin.tenant_id.is_some() && pin.instance_id.is_some())
            && self.valid_pinned_benchmark(cx)
            && self.pinned_benchmark.as_ref().is_some_and(|pin| {
                pin.request.sql == request.baseline.sql
                    && pin.request.params == request.baseline.params
            });
        if !pin_valid {
            self.show_error_toast(
                "Baseline source, bindings or connection changed; pin and review again".into(),
                cx,
            );
            return false;
        }
        if !self.require_operation(
            sift_protocol::OperationKind::BenchmarkPair,
            "Run paired benchmark",
            cx,
        ) {
            return false;
        }
        let Some(PerformanceReviewRequest::Pair(request)) = self
            .pending_performance_review
            .as_mut()
            .map(|review| &mut review.request)
        else {
            return false;
        };
        if matches!(
            request.order,
            sift_protocol::BenchmarkPairOrder::RandomizedV1 { .. }
        ) {
            let seed = self
                .paired_seed_input
                .as_ref()
                .and_then(|input| input.read(cx).text().parse::<u64>().ok());
            let Some(seed) = seed else {
                self.show_error_toast("Enter an unsigned 64-bit seed, including zero".into(), cx);
                return false;
            };
            request.order = sift_protocol::BenchmarkPairOrder::RandomizedV1 { seed };
        }
        true
    }
    pub(super) fn paired_review_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .paired_seed_input
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window))
        {
            if event.keystroke.key == "escape" {
                self.focus_handle.focus(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        match event.keystroke.key.as_str() {
            "a" => self.pin_performance_workload(window, cx),
            "m" => self.cycle_paired_order(cx),
            "v" => self.cycle_paired_verification(cx),
            "i" => {
                if let Some(input) = &self.paired_seed_input {
                    input.focus_handle(cx).focus(window, cx);
                }
            }
            "x" => self.confirm_performance_review(cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn paired_execution_source(&self, run_id: uuid::Uuid) -> MonitorActionSource {
        let pin = self
            .pinned_benchmark
            .as_ref()
            .expect("paired confirmation validated its anchor");
        MonitorActionSource {
            instance_id: pin.instance_id.clone().expect("connected instance"),
            tenant_id: pin.tenant_id.expect("paired confirmation validated tenant"),
            profile_id: pin.profile_id,
            session_id: pin.identity.session,
            connection_id: pin.identity.connection,
            request_id: run_id,
        }
    }
    pub(super) fn pair_source_context(&self) -> Option<sift_protocol::BenchmarkSourceContext> {
        self.pinned_benchmark
            .as_ref()
            .and_then(|pin| pin.source_context.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    #[gpui::test]
    fn vim_pair_review_pins_without_execution_and_rejects_edited_anchor(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        cx.update(|_, cx| {
            cx.bind_keys([
                gpui::KeyBinding::new(
                    "r",
                    crate::results::RunBenchmark,
                    Some("SiftPerformance && !SiftTextInput"),
                ),
                gpui::KeyBinding::new(
                    "[",
                    crate::results::ShowPairBaseline,
                    Some("SiftPerformance && !SiftTextInput"),
                ),
                gpui::KeyBinding::new(
                    "]",
                    crate::results::ShowPairCandidate,
                    Some("SiftPerformance && !SiftTextInput"),
                ),
            ])
        });
        let (sender, mut commands) = ExecutorSender::channel(16);
        workspace.update(&mut cx, |shell, _| {
            shell.executor_sender = Some(sender);
            shell.selected_instance_id = Some("local".into());
            shell.connected_profiles.insert(2);
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 2,
                name: "paired".into(),
            };
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Local".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.performance_connection_ids.insert(
                2,
                PerformanceConnectionIdentity {
                    session: sift_protocol::SessionId(21),
                    connection: sift_protocol::ConnectionId(22),
                    generation: uuid::Uuid::new_v4(),
                },
            );
        });
        let add = |sql: &str, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |shell, window, cx| {
                shell.new_query(window, cx);
                let pane = shell.panes[shell.active_pane].clone();
                let item_id = pane.read(cx).active_item().unwrap().id;
                let editor = pane.read(cx).editor(item_id).unwrap();
                editor.update(cx, |editor, cx| editor.replace_text_from_owner(sql, cx));
                shell.query_semantic_targets.insert(
                    item_id,
                    SemanticConnectionTarget {
                        instance_id: "local".into(),
                        tenant_id: 1,
                        profile_id: 2,
                        profile_name: "paired".into(),
                        provider_id: sift_protocol::Engine::Postgres.provider_id(),
                        database: None,
                    },
                );
                let results = pane.read(cx).results.get(&item_id).unwrap().clone();
                results.update(cx, |view, cx| view.show_performance(cx));
                results.read(cx).focus_handle(cx).focus(window, cx);
                (item_id, editor, results)
            })
        };
        let (a_id, a_editor, _) = add("SELECT 10", &mut cx);
        cx.run_until_parked();
        cx.simulate_keystrokes("r a");
        assert!(
            commands.try_recv().is_err(),
            "pinning must not dispatch a workload"
        );
        workspace.read_with(&cx, |shell, _| {
            assert!(shell.pinned_benchmark.is_some());
            assert!(shell.pending_performance_review.is_none());
        });
        let (b_id, _, b_results) = add("SELECT 20", &mut cx);
        cx.run_until_parked();
        cx.simulate_keystrokes("r m m v v");
        assert!(
            commands.try_recv().is_err(),
            "review controls must not execute"
        );
        workspace.update(&mut cx, |shell, cx| {
            shell
                .paired_seed_input
                .as_ref()
                .unwrap()
                .update(cx, |input, cx| input.set_text("0", cx))
        });
        cx.simulate_keystrokes("i");
        assert!(workspace.update_in(&mut cx, |shell, window, cx| shell
            .paired_seed_input
            .as_ref()
            .unwrap()
            .focus_handle(cx)
            .is_focused(window)));
        cx.simulate_keystrokes("escape x");
        let (source, request) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkPair {
                item_id,
                source,
                request,
            } => {
                assert_eq!(item_id, b_id);
                (source, request)
            }
            _ => panic!("expected controlled pair"),
        };
        assert_eq!(source.instance_id, "local");
        assert_eq!(source.tenant_id, 1);
        assert_eq!(source.session_id, sift_protocol::SessionId(21));
        assert_eq!(request.baseline.sql, "SELECT 10");
        assert_eq!(request.candidate.sql, "SELECT 20");
        assert_eq!(
            request.order,
            sift_protocol::BenchmarkPairOrder::RandomizedV1 { seed: 0 }
        );
        assert!(request.workload_confirmed);
        assert_eq!(
            request.verify_results.unwrap().order,
            sift_protocol::BenchmarkResultOrder::Unordered
        );
        let order = sift_core::performance_pair::schedule(&request.limits, request.order).unwrap();
        let report = |population: sift_protocol::BenchmarkPopulation, sql: &str| {
            let run_id = if population == sift_protocol::BenchmarkPopulation::Baseline {
                request.run_id
            } else {
                uuid::Uuid::new_v4()
            };
            let samples=(0..request.limits.warmups+request.limits.iterations).map(|ordinal|serde_json::json!({"ordinal":ordinal,"warmup":ordinal<request.limits.warmups,"outcome":"success","elapsed_ns":100,"client_elapsed_ns":100,"rows":1})).collect::<Vec<_>>();
            serde_json::from_value::<sift_protocol::BenchmarkReport>(serde_json::json!({"version":2,"run_id":run_id,"engine":"postgres","sql":sql,"captured_at":chrono::Utc::now(),"warmups":request.limits.warmups,"requested_iterations":request.limits.iterations,"query_timeout_ms":request.limits.query_timeout_ms,"total_budget_ms":request.limits.total_budget_ms,"delay_ms":0,"parameter_count":0,"pair_context":{"version":1,"run_id":request.run_id,"population":population,"order":request.order,"actual_order":order,"pair_completed":true},"samples":samples,"completed":true,"warnings":[],"median_ns":100,"mean_ns":100,"min_ns":100,"max_ns":100,"standard_deviation_ns":0,"p95_ns":null,"p99_ns":null})).unwrap()
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::BenchmarkPairFinished {
                    item_id: b_id,
                    run_id: request.run_id,
                    response: Ok(Box::new(sift_protocol::BenchmarkPairReport {
                        baseline: report(sift_protocol::BenchmarkPopulation::Baseline, "SELECT 10"),
                        candidate: report(
                            sift_protocol::BenchmarkPopulation::Candidate,
                            "SELECT 20",
                        ),
                    })),
                },
                cx,
            )
        });
        workspace.update_in(&mut cx, |_, window, cx| {
            b_results.read(cx).focus_handle(cx).focus(window, cx)
        });
        cx.simulate_keystrokes("[");
        assert_eq!(
            b_results.read_with(&cx, |view, _| view.benchmark_report().unwrap().sql),
            "SELECT 10"
        );
        cx.simulate_keystrokes("] r m");
        a_editor.update(&mut cx, |editor, cx| {
            editor.replace_text_from_owner("SELECT 11", cx)
        });
        cx.simulate_keystrokes("x");
        assert!(
            commands.try_recv().is_err(),
            "editing A invalidates paired confirmation"
        );
        workspace.update(&mut cx, |shell, _| {
            shell.observe_pinned_workload_execution(b_id, "SELECT 10", &[]);
            assert!(
                shell.pinned_benchmark.is_some(),
                "another tab cannot replace A's frozen binds"
            );
            shell.observe_pinned_workload_execution(
                a_id,
                "SELECT 10",
                &[sift_protocol::Value::Int64(2)],
            );
            assert!(
                shell.pinned_benchmark.is_none(),
                "binding changes at A invalidate its anchor"
            );
        });
    }
}
