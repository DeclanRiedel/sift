//! Atomic principal + tenant token-bucket admission (ADR-020).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sift_protocol::RateLimitClass;

use crate::config::{RateBucketConfig, RateLimitsConfig};

#[derive(Clone, Default)]
pub struct RateLimiter {
    inner: Arc<Mutex<Inner>>,
    config: Arc<LimiterConfig>,
}

#[derive(Default)]
struct Inner {
    buckets: HashMap<BucketKey, Bucket>,
    admissions: u64,
}

#[derive(Clone, Default)]
struct LimiterConfig {
    trusted_local_exempt: bool,
    idle_ttl: Duration,
    classes: HashMap<RateLimitClass, RateBucketConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BucketKey {
    Principal(i64, RateLimitClass),
    Tenant(i64, RateLimitClass),
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated_at: Instant,
    last_used: Instant,
}

impl RateLimiter {
    pub fn from_config(config: &RateLimitsConfig) -> Self {
        let mut classes = HashMap::new();
        for (class, bucket) in [
            (RateLimitClass::Control, config.control.as_ref()),
            (RateLimitClass::Interactive, config.interactive.as_ref()),
            (RateLimitClass::Query, config.query.as_ref()),
            (
                RateLimitClass::HeavyTransfer,
                config.heavy_transfer.as_ref(),
            ),
            (RateLimitClass::StreamBytes, config.stream_bytes.as_ref()),
        ] {
            if let Some(bucket) = bucket {
                classes.insert(class, bucket.clone());
            }
        }
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            config: Arc::new(LimiterConfig {
                trusted_local_exempt: config.trusted_local_exempt,
                idle_ttl: Duration::from_secs(config.idle_ttl_secs.max(1)),
                classes,
            }),
        }
    }

    pub fn admit(
        &self,
        principal_id: i64,
        tenant_id: Option<i64>,
        class: RateLimitClass,
        trusted_local: bool,
    ) -> Result<(), u64> {
        let Some(bucket_config) = self.config.classes.get(&class) else {
            return Ok(());
        };
        if trusted_local && self.config.trusted_local_exempt {
            return Ok(());
        }
        self.admit_at(
            principal_id,
            tenant_id,
            class,
            bucket_config.cost,
            Instant::now(),
        )
    }

    pub async fn pace_bytes(
        &self,
        principal_id: i64,
        tenant_id: Option<i64>,
        bytes: usize,
        trusted_local: bool,
        max_wait: Duration,
    ) -> Result<(), u64> {
        if trusted_local && self.config.trusted_local_exempt {
            return Ok(());
        }
        let class = RateLimitClass::StreamBytes;
        let Some(config) = self.config.classes.get(&class) else {
            return Ok(());
        };
        let started = Instant::now();
        loop {
            match self.admit_at(principal_id, tenant_id, class, bytes as f64, Instant::now()) {
                Ok(()) => return Ok(()),
                Err(retry) if started.elapsed() + Duration::from_secs(retry) <= max_wait => {
                    tokio::time::sleep(Duration::from_secs(retry)).await;
                }
                Err(retry) => return Err(retry),
            }
            if bytes as f64 > config.burst {
                return Err(((bytes as f64 - config.burst) / config.refill_per_second)
                    .ceil()
                    .max(1.0) as u64);
            }
        }
    }

    fn admit_at(
        &self,
        principal_id: i64,
        tenant_id: Option<i64>,
        class: RateLimitClass,
        cost: f64,
        now: Instant,
    ) -> Result<(), u64> {
        let Some(config) = self.config.classes.get(&class) else {
            return Ok(());
        };
        if cost > config.burst {
            return Err(((cost - config.burst) / config.refill_per_second)
                .ceil()
                .max(1.0) as u64);
        }
        let mut inner = self.inner.lock().unwrap();
        inner.admissions = inner.admissions.wrapping_add(1);
        if inner.admissions % 256 == 0 {
            let idle_ttl = self.config.idle_ttl;
            inner
                .buckets
                .retain(|_, bucket| now.saturating_duration_since(bucket.last_used) < idle_ttl);
        }
        let mut keys = vec![BucketKey::Principal(principal_id, class)];
        if let Some(tenant_id) = tenant_id {
            keys.push(BucketKey::Tenant(tenant_id, class));
        }
        let candidates: Vec<_> = keys
            .iter()
            .map(|key| {
                let current = inner.buckets.get(key).copied().unwrap_or(Bucket {
                    tokens: config.burst,
                    updated_at: now,
                    last_used: now,
                });
                let elapsed = now
                    .saturating_duration_since(current.updated_at)
                    .as_secs_f64();
                let tokens =
                    (current.tokens + elapsed * config.refill_per_second).min(config.burst);
                (*key, tokens)
            })
            .collect();
        let retry_after = candidates
            .iter()
            .filter(|(_, tokens)| *tokens < cost)
            .map(|(_, tokens)| {
                ((cost - *tokens) / config.refill_per_second)
                    .ceil()
                    .max(1.0) as u64
            })
            .max();
        if let Some(retry_after) = retry_after {
            return Err(retry_after);
        }
        for (key, tokens) in candidates {
            inner.buckets.insert(
                key,
                Bucket {
                    tokens: tokens - cost,
                    updated_at: now,
                    last_used: now,
                },
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter() -> RateLimiter {
        let config = RateLimitsConfig {
            trusted_local_exempt: false,
            query: Some(RateBucketConfig {
                refill_per_second: 1.0,
                burst: 2.0,
                cost: 1.0,
            }),
            ..RateLimitsConfig::default()
        };
        RateLimiter::from_config(&config)
    }

    #[test]
    fn burst_refill_and_retry_are_deterministic() {
        let limiter = limiter();
        let now = Instant::now();
        assert!(limiter
            .admit_at(1, Some(10), RateLimitClass::Query, 1.0, now)
            .is_ok());
        assert!(limiter
            .admit_at(1, Some(10), RateLimitClass::Query, 1.0, now)
            .is_ok());
        assert_eq!(
            limiter.admit_at(1, Some(10), RateLimitClass::Query, 1.0, now),
            Err(1)
        );
        assert!(limiter
            .admit_at(
                1,
                Some(10),
                RateLimitClass::Query,
                1.0,
                now + Duration::from_secs(1),
            )
            .is_ok());
    }

    #[test]
    fn tenant_denial_does_not_charge_principal() {
        let limiter = limiter();
        let now = Instant::now();
        for principal in [1, 2] {
            assert!(limiter
                .admit_at(principal, Some(10), RateLimitClass::Query, 1.0, now)
                .is_ok());
        }
        assert_eq!(
            limiter.admit_at(1, Some(10), RateLimitClass::Query, 1.0, now),
            Err(1)
        );
        assert!(limiter
            .admit_at(1, Some(11), RateLimitClass::Query, 1.0, now)
            .is_ok());
    }

    #[test]
    fn trusted_local_exemption_does_not_consume_buckets() {
        let mut config = RateLimitsConfig {
            trusted_local_exempt: true,
            query: Some(RateBucketConfig {
                refill_per_second: 1.0,
                burst: 2.0,
                cost: 1.0,
            }),
            ..RateLimitsConfig::default()
        };
        let limiter = RateLimiter::from_config(&config);
        for _ in 0..4 {
            assert!(limiter
                .admit(1, Some(10), RateLimitClass::Query, true)
                .is_ok());
        }
        assert!(limiter
            .admit(1, Some(10), RateLimitClass::Query, false)
            .is_ok());
        assert!(limiter
            .admit(1, Some(10), RateLimitClass::Query, false)
            .is_ok());
        assert!(limiter
            .admit(1, Some(10), RateLimitClass::Query, false)
            .is_err());

        config.trusted_local_exempt = false;
        let constrained = RateLimiter::from_config(&config);
        assert!(constrained
            .admit(1, Some(10), RateLimitClass::Query, true)
            .is_ok());
    }
}
