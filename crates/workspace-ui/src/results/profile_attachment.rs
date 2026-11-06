//! Ephemeral workload association. Salt/digests and bind values never enter saved JSON.
use super::*;
use sha2::{Digest, Sha256};

pub(super) struct Workload {
    run_id: uuid::Uuid,
    captured_at: chrono::DateTime<chrono::Utc>,
    sql: String,
    parameter_count: usize,
    digest: [u8; 32],
}
impl Workload {
    fn freeze(
        salt: uuid::Uuid,
        run_id: uuid::Uuid,
        source: &str,
        sql: &str,
        params: &[Value],
    ) -> Option<Self> {
        if params.iter().any(|value| match value {
            Value::Float32(v) => !v.is_finite(),
            Value::Float64(v) => !v.is_finite(),
            _ => false,
        }) {
            return None;
        }
        let mut hash = Sha256::new();
        hash.update(salt.as_bytes());
        hash.update(serde_json::to_vec(&(source, sql, params)).ok()?);
        Some(Self {
            run_id,
            captured_at: chrono::Utc::now(),
            sql: sql.into(),
            parameter_count: params.len(),
            digest: hash.finalize().into(),
        })
    }
}
impl ResultsView {
    pub(crate) fn freeze_performance_workload(
        &mut self,
        run_id: uuid::Uuid,
        source: &str,
        sql: &str,
        params: &[Value],
    ) {
        let workload = Workload::freeze(self.workload_salt, run_id, source, sql, params);
        if self.benchmark_pending == Some(run_id) {
            self.benchmark_pending_workload = workload;
        } else if self.profile_pending == Some(run_id) {
            self.profile_pending_workload = workload;
        }
    }
    pub(crate) fn matching_profile_attachment(
        &self,
    ) -> Option<sift_protocol::BenchmarkProfileAttachment> {
        if self.benchmark_pending.is_some() || self.profile_pending.is_some() {
            return None;
        }
        let benchmark = self.benchmark_workload.as_ref()?;
        let profile = self.profile_workload.as_ref()?;
        let report = self.benchmark_report.as_ref()?;
        let response = self.profile_result.as_ref()?;
        if benchmark.digest != profile.digest
            || benchmark.run_id != report.run_id
            || profile.run_id != response.run_id
            || benchmark.sql != report.sql
        {
            return None;
        }
        let attachment = sift_protocol::BenchmarkProfileAttachment {
            captured_at: profile.captured_at,
            sql: profile.sql.clone(),
            parameter_count: profile.parameter_count,
            response: response.clone(),
        };
        attachment.validate_for(report).ok()?;
        Some(attachment)
    }
    pub(crate) fn show_saved_profile(
        &mut self,
        attachment: sift_protocol::BenchmarkProfileAttachment,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.profile_pending.is_some() || self.benchmark_pending.is_some() {
            return false;
        }
        self.profile_pending = None;
        self.profile_pending_workload = None;
        self.profile_workload = None;
        self.replace_profile_plan(attachment.response, true);
        self.profile_error = None;
        self.show_performance(cx);
        self.performance_section = PerformanceSection::Plan;
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(run_id: uuid::Uuid) -> sift_protocol::BenchmarkReport {
        serde_json::from_value(serde_json::json!({"version":2,"run_id":run_id,"engine":"postgres","sql":"SELECT $1","captured_at":chrono::Utc::now(),"parameter_count":1,"warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"samples":[],"completed":false,"warnings":[]})).unwrap()
    }
    fn response(run_id: uuid::Uuid) -> sift_protocol::ProfileResponse {
        serde_json::from_value(serde_json::json!({"run_id":run_id,"plan":{"engine":"postgres","analyzed":true,"raw":"private-plan","root":{"op":"Result","actual_rows":1.0}},"server_elapsed_ns":100,"execution_ms":1.0})).unwrap()
    }
    #[gpui::test]
    fn profile_attachment_requires_frozen_values_and_source_and_never_executes(
        cx: &mut gpui::TestAppContext,
    ) {
        let entity = cx.new(ResultsView::new);
        entity.update(cx, |view, cx| {
            let benchmark = uuid::Uuid::new_v4();
            let profile = uuid::Uuid::new_v4();
            let params = [Value::Text("private-bind-sentinel".into())];
            let source = "server/profile/session/connection/generation-1";
            view.benchmark_pending = Some(benchmark);
            view.freeze_performance_workload(benchmark, source, "SELECT $1", &params);
            view.set_benchmark_result(benchmark, Ok(report(benchmark)), cx);
            view.profile_pending = Some(profile);
            view.freeze_performance_workload(profile, source, "SELECT $1", &params);
            view.set_profile_result(profile, Ok(response(profile)), cx);
            let attachment = view
                .matching_profile_attachment()
                .expect("matching confirmed workload");
            assert!(!serde_json::to_string(&attachment)
                .unwrap()
                .contains("private-bind-sentinel"));
            assert!(view.benchmark_report.as_ref().unwrap().samples.is_empty());
            for (changed_source, sql, values) in [
                (
                    "other-server/profile/session/connection/generation-1",
                    "SELECT $1",
                    params.to_vec(),
                ),
                (
                    "server/profile/session/connection/generation-2",
                    "SELECT $1",
                    params.to_vec(),
                ),
                (source, "SELECT $1 + 1", params.to_vec()),
                (source, "SELECT $1", vec![Value::Text("changed".into())]),
            ] {
                let id = uuid::Uuid::new_v4();
                view.profile_pending = Some(id);
                view.freeze_performance_workload(id, changed_source, sql, &values);
                view.set_profile_result(id, Ok(response(id)), cx);
                assert!(view.matching_profile_attachment().is_none());
            }
            let next = uuid::Uuid::new_v4();
            view.profile_pending = Some(next);
            view.freeze_performance_workload(next, source, "SELECT $1", &params);
            view.set_profile_result(profile, Ok(response(profile)), cx);
            assert_eq!(view.profile_pending, Some(next));
            assert!(
                !view.show_saved_profile(attachment.clone(), cx),
                "opening must not clear a live job"
            );
            view.set_profile_result(next, Ok(response(profile)), cx);
            assert!(
                view.matching_profile_attachment().is_none(),
                "wrong response identity cannot create association"
            );
            let next = uuid::Uuid::new_v4();
            view.profile_pending = Some(next);
            view.freeze_performance_workload(next, source, "SELECT $1", &params);
            view.set_profile_result(next, Err("refused".into()), cx);
            assert!(view.matching_profile_attachment().is_none());
            assert!(view.show_saved_profile(attachment, cx));
            assert!(view.profile_is_saved);
            assert!(view.profile_pending.is_none() && view.benchmark_pending.is_none());
            assert!(
                view.matching_profile_attachment().is_none(),
                "historical open has no live association"
            );
            assert_eq!(view.performance_section, PerformanceSection::Plan);
            let a = Workload::freeze(
                uuid::Uuid::new_v4(),
                benchmark,
                source,
                "SELECT $1",
                &params,
            )
            .unwrap();
            let b = Workload::freeze(
                uuid::Uuid::new_v4(),
                benchmark,
                source,
                "SELECT $1",
                &params,
            )
            .unwrap();
            assert_ne!(a.digest, b.digest);
            assert!(Workload::freeze(
                uuid::Uuid::new_v4(),
                benchmark,
                source,
                "SELECT $1",
                &[Value::Float64(f64::NAN)]
            )
            .is_none());
        });
    }
}
