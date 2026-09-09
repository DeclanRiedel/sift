use crate::{parse_time_sql, MetadataError, MetadataStore, PrincipalId, Result};
use chrono::{Timelike, Utc};
use rusqlite::params;
use sift_api_types::{QueryPerformanceBucket, QueryPerformanceSummary};
use std::collections::BTreeMap;

impl MetadataStore {
    /// Summarize only this principal's durable history; never return SQL or binds.
    pub fn query_performance(
        &self,
        actor: PrincipalId,
        days: u32,
        profile_id: Option<i64>,
    ) -> Result<QueryPerformanceSummary> {
        if !(1..=30).contains(&days) || profile_id.is_some_and(|id| id <= 0) {
            return Err(MetadataError::InvalidEnum {
                field: "performance_query",
                value: "lookback must be 1..30 days and profile id positive".into(),
            });
        }
        let now = Utc::now();
        let cutoff = (now - chrono::Duration::days(i64::from(days))).to_rfc3339();
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT started_at, duration_ms, status FROM query_history WHERE principal_id = ?1 AND started_at >= ?2 AND (?3 IS NULL OR connection_profile_id = ?3) ORDER BY started_at DESC, id DESC LIMIT 10001")?;
        let rows = stmt
            .query_map(params![actor.0, cutoff, profile_id], |row| {
                Ok((
                    parse_time_sql(row.get(0)?)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let truncated = rows.len() > 10000;
        let count = rows.len().min(10000);
        let mut buckets = BTreeMap::<_, (QueryPerformanceBucket, Vec<u64>)>::new();
        for (at, duration, status) in rows.into_iter().take(10000) {
            let hour = at
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();
            let (bucket, durations) = buckets.entry(hour).or_insert_with(|| {
                (
                    QueryPerformanceBucket {
                        hour,
                        executions: 0,
                        errors: 0,
                        canceled: 0,
                        timed_executions: 0,
                        mean_duration_ms: None,
                        p95_duration_ms: None,
                        max_duration_ms: None,
                    },
                    vec![],
                )
            });
            bucket.executions += 1;
            bucket.errors += u64::from(status == "error");
            bucket.canceled += u64::from(status == "canceled");
            if let Some(duration) = duration.and_then(|d| u64::try_from(d).ok()) {
                durations.push(duration);
            }
        }
        let buckets = buckets
            .into_values()
            .map(|(mut bucket, mut durations)| {
                durations.sort_unstable();
                bucket.timed_executions = durations.len() as u64;
                if !durations.is_empty() {
                    bucket.mean_duration_ms = Some(
                        durations.iter().map(|d| *d as f64).sum::<f64>() / durations.len() as f64,
                    );
                    bucket.p95_duration_ms =
                        Some(durations[(durations.len() * 95).div_ceil(100) - 1]);
                    bucket.max_duration_ms = durations.last().copied();
                }
                bucket
            })
            .collect();
        Ok(QueryPerformanceSummary {
            generated_at: now,
            lookback_days: days,
            sampled_executions: count,
            truncated,
            buckets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemorySecretStore, NewQueryHistory, QueryStatus};
    #[test]
    fn performance_summary_excludes_payloads_and_other_principals() {
        let store =
            MetadataStore::open_in_memory(std::sync::Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("test").unwrap();
        for (duration, status) in [
            (Some(10), QueryStatus::Ok),
            (Some(30), QueryStatus::Error),
            (None, QueryStatus::Canceled),
        ] {
            store
                .record_query_history(NewQueryHistory {
                    principal_id: PrincipalId(1),
                    room_id: None,
                    connection_profile_id: None,
                    sql_text: "secret query".into(),
                    duration_ms: duration,
                    row_count: None,
                    status,
                    error_code: None,
                    error_message: Some("private failure".into()),
                    variable_descriptors: vec![],
                })
                .unwrap();
        }
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE query_history SET started_at = ?1",
                params![Utc::now().to_rfc3339()],
            )
            .unwrap();
        let report = store.query_performance(PrincipalId(1), 7, None).unwrap();
        assert_eq!(report.sampled_executions, 3);
        assert_eq!(report.buckets[0].mean_duration_ms, Some(20.0));
        assert_eq!(report.buckets[0].p95_duration_ms, Some(30));
        assert_eq!(
            (report.buckets[0].errors, report.buckets[0].canceled),
            (1, 1)
        );
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("secret query"));
        assert_eq!(
            store
                .query_performance(PrincipalId(999), 7, None)
                .unwrap()
                .sampled_executions,
            0
        );
        assert!(store.query_performance(PrincipalId(1), 31, None).is_err());
    }

    #[test]
    fn performance_caps_samples_and_excludes_old_history() {
        let store =
            MetadataStore::open_in_memory(std::sync::Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("test").unwrap();
        {
            let conn = store.conn().unwrap();
            conn.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 10001) INSERT INTO query_history (principal_id, sql_text, started_at, duration_ms, status) SELECT 1, 'private', ?1, x, 'ok' FROM n", params![Utc::now().to_rfc3339()]).unwrap();
        }
        let report = store.query_performance(PrincipalId(1), 1, None).unwrap();
        assert!(report.truncated);
        assert_eq!(report.sampled_executions, 10000);
        assert_eq!(
            store
                .query_performance(PrincipalId(1), 1, Some(999))
                .unwrap()
                .sampled_executions,
            0
        );
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE query_history SET started_at = ?1",
                params![(Utc::now() - chrono::Duration::days(31)).to_rfc3339()],
            )
            .unwrap();
        let report = store.query_performance(PrincipalId(1), 30, None).unwrap();
        assert_eq!(report.sampled_executions, 0);
        assert!(!report.truncated);
    }
}
