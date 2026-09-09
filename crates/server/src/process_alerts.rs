//! Bounded, payload-free alert transitions over audited process samples.
use crate::error::{ApiError, ApiResult};
use chrono::{DateTime, Utc};
use sift_protocol::{DatabaseProcess, ProcessAlert, ProcessAlertKind, ProcessAlertSample};
use std::collections::HashMap;

#[derive(Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AlertOptions {
    pub long_query_seconds: u64,
    pub idle_transaction_seconds: u64,
    pub poll_seconds: u64,
    pub duration_seconds: u64,
}

impl Default for AlertOptions {
    fn default() -> Self {
        Self {
            long_query_seconds: 60,
            idle_transaction_seconds: 300,
            poll_seconds: 5,
            duration_seconds: 3600,
        }
    }
}

impl AlertOptions {
    pub fn validate(&self) -> ApiResult<()> {
        if self.long_query_seconds > 86400
            || self.idle_transaction_seconds > 86400
            || self.long_query_seconds == 0 && self.idle_transaction_seconds == 0
            || !(2..=300).contains(&self.poll_seconds)
            || !(1..=3600).contains(&self.duration_seconds)
        {
            return Err(ApiError::BadRequest("alert thresholds must be 0..86400 seconds (not both disabled), poll 2..300 seconds, duration 1..3600 seconds".into()));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct AlertTracker {
    active: HashMap<(i64, ProcessAlertKind, DateTime<Utc>), ProcessAlert>,
}

impl AlertTracker {
    pub fn sample(
        &mut self,
        processes: &[DatabaseProcess],
        options: &AlertOptions,
        now: DateTime<Utc>,
    ) -> ProcessAlertSample {
        let mut sample = ProcessAlertSample {
            sampled_at: now,
            observed_processes: processes.len(),
            incomplete: processes.len() >= 500,
            changes: vec![],
        };
        // A truncated snapshot cannot prove that previous conditions resolved.
        // Leave bounded prior state untouched until a complete sample arrives.
        if sample.incomplete {
            return sample;
        }
        let mut active = HashMap::new();
        for process in processes {
            let state = process
                .state
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let candidate = if matches!(
                state.as_str(),
                "active" | "running" | "runnable" | "suspended"
            ) {
                process.started_at.map(|start| {
                    (
                        ProcessAlertKind::LongRunningQuery,
                        start,
                        options.long_query_seconds,
                    )
                })
            } else if matches!(
                state.as_str(),
                "idle in transaction" | "idle in transaction (aborted)"
            ) {
                process
                    .transaction_started_at
                    .zip(process.state_changed_at)
                    .map(|(transaction, idle)| {
                        (
                            ProcessAlertKind::IdleInTransaction,
                            transaction.max(idle),
                            options.idle_transaction_seconds,
                        )
                    })
            } else {
                None
            };
            let Some((kind, started_at, threshold)) = candidate else {
                continue;
            };
            let age = now.signed_duration_since(started_at).num_seconds();
            if threshold == 0 || age < 0 || (age as u64) < threshold {
                continue;
            }
            let key = (process.process_id, kind, started_at);
            let alert = ProcessAlert {
                kind,
                process_id: process.process_id,
                started_at,
                age_seconds: age as u64,
                active: true,
            };
            if !self.active.contains_key(&key) {
                sample.changes.push(alert.clone());
            }
            active.insert(key, alert);
        }
        for (key, mut alert) in self.active.drain() {
            if !active.contains_key(&key) {
                alert.active = false;
                alert.age_seconds = now
                    .signed_duration_since(alert.started_at)
                    .num_seconds()
                    .max(0) as u64;
                sample.changes.push(alert);
            }
        }
        self.active = active;
        sample
            .changes
            .sort_by_key(|alert| (alert.process_id, alert.started_at, alert.active));
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alerts_deduplicate_resolve_and_measure_idle_time_separately() {
        let now = Utc::now();
        let mut process = DatabaseProcess {
            engine: sift_protocol::Engine::Postgres,
            process_id: 7,
            user: None,
            database: None,
            state: Some("active".into()),
            statement: Some("secret SQL must not enter alerts".into()),
            started_at: Some(now - chrono::Duration::seconds(70)),
            transaction_started_at: Some(now - chrono::Duration::hours(1)),
            state_changed_at: Some(now - chrono::Duration::seconds(10)),
            wait: None,
            blocked_by: vec![],
        };
        let options = AlertOptions::default();
        let mut tracker = AlertTracker::default();
        let first = tracker.sample(&[process.clone()], &options, now);
        assert_eq!(first.changes.len(), 1);
        assert!(!serde_json::to_string(&first)
            .unwrap()
            .contains("secret SQL"));
        assert!(tracker
            .sample(&[process.clone()], &options, now)
            .changes
            .is_empty());
        assert!(
            tracker
                .sample(&vec![process.clone(); 500], &options, now)
                .incomplete
        );
        process.state = Some("idle in transaction".into());
        let changed = tracker.sample(&[process.clone()], &options, now);
        assert_eq!(changed.changes.len(), 1);
        assert!(!changed.changes[0].active);
        process.state_changed_at = Some(now - chrono::Duration::seconds(301));
        let idle = tracker.sample(&[process], &options, now);
        assert_eq!(idle.changes[0].kind, ProcessAlertKind::IdleInTransaction);
        assert_eq!(idle.changes[0].age_seconds, 301);
        assert!(!tracker.sample(&[], &options, now).changes[0].active);
    }
}
