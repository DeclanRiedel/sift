use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sift_extension_protocol::ExtensionId;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct RestartPolicy {
    pub maximum_failures: usize,
    pub window: Duration,
    pub initial_backoff: Duration,
    pub maximum_backoff: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            maximum_failures: 5,
            window: Duration::from_secs(10 * 60),
            initial_backoff: Duration::from_millis(250),
            maximum_backoff: Duration::from_secs(30),
        }
    }
}

pub struct RestartBudget {
    policy: RestartPolicy,
    failures: VecDeque<Instant>,
}

impl RestartBudget {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            failures: VecDeque::new(),
        }
    }

    pub fn record_failure(&mut self, now: Instant) -> Option<Duration> {
        while self
            .failures
            .front()
            .is_some_and(|failure| now.duration_since(*failure) >= self.policy.window)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        if self.failures.len() > self.policy.maximum_failures {
            return None;
        }
        let exponent = self.failures.len().saturating_sub(1).min(31) as u32;
        let multiplier = 1_u32 << exponent;
        let base = self
            .policy
            .initial_backoff
            .saturating_mul(multiplier)
            .min(self.policy.maximum_backoff);
        Some(add_jitter(base))
    }

    pub fn reset(&mut self) {
        self.failures.clear();
    }
}

fn add_jitter(base: Duration) -> Duration {
    let maximum_jitter_ms = (base.as_millis() / 5).min(u64::MAX as u128) as u64;
    if maximum_jitter_ms == 0 {
        return base;
    }
    let mut random = [0_u8; 8];
    if getrandom::getrandom(&mut random).is_err() {
        return base;
    }
    base.saturating_add(Duration::from_millis(
        u64::from_le_bytes(random) % (maximum_jitter_ms + 1),
    ))
}

#[derive(Debug, Clone)]
pub struct GenerationLimits {
    pub per_extension_tenants: usize,
    pub per_instance: usize,
    pub per_extension_tenant: usize,
}

impl Default for GenerationLimits {
    fn default() -> Self {
        Self {
            per_extension_tenants: 32,
            per_instance: 256,
            per_extension_tenant: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GenerationKey {
    pub extension_id: ExtensionId,
    pub tenant_id: i64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GenerationAdmissionError {
    #[error("instance extension-process limit reached")]
    InstanceLimit,
    #[error("extension tenant-process limit reached")]
    ExtensionLimit,
    #[error("active and upgrade-candidate slots are already occupied")]
    TenantLimit,
}

#[derive(Clone)]
pub struct GenerationLimiter {
    limits: GenerationLimits,
    state: Arc<Mutex<GenerationCounts>>,
}

#[derive(Default)]
struct GenerationCounts {
    total: usize,
    by_extension: HashMap<ExtensionId, usize>,
    by_key: HashMap<GenerationKey, usize>,
}

pub struct GenerationPermit {
    state: Arc<Mutex<GenerationCounts>>,
    key: GenerationKey,
}

impl GenerationLimiter {
    pub fn new(limits: GenerationLimits) -> Self {
        Self {
            limits,
            state: Arc::new(Mutex::new(GenerationCounts::default())),
        }
    }

    pub fn acquire(
        &self,
        key: GenerationKey,
    ) -> Result<GenerationPermit, GenerationAdmissionError> {
        let mut state = self.state.lock().expect("generation limiter poisoned");
        if state.total >= self.limits.per_instance {
            return Err(GenerationAdmissionError::InstanceLimit);
        }
        if state.by_key.get(&key).copied().unwrap_or(0) >= self.limits.per_extension_tenant {
            return Err(GenerationAdmissionError::TenantLimit);
        }
        let extension_count = state
            .by_extension
            .get(&key.extension_id)
            .copied()
            .unwrap_or(0);
        let tenant_already_present = state.by_key.contains_key(&key);
        if !tenant_already_present && extension_count >= self.limits.per_extension_tenants {
            return Err(GenerationAdmissionError::ExtensionLimit);
        }
        state.total += 1;
        if !tenant_already_present {
            *state
                .by_extension
                .entry(key.extension_id.clone())
                .or_default() += 1;
        }
        *state.by_key.entry(key.clone()).or_default() += 1;
        Ok(GenerationPermit {
            state: self.state.clone(),
            key,
        })
    }

    pub fn active(&self) -> usize {
        self.state
            .lock()
            .expect("generation limiter poisoned")
            .total
    }
}

impl Drop for GenerationPermit {
    fn drop(&mut self) {
        let mut state = self.state.lock().expect("generation limiter poisoned");
        state.total = state.total.saturating_sub(1);
        let remove_key = if let Some(count) = state.by_key.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            *count == 0
        } else {
            false
        };
        if remove_key {
            state.by_key.remove(&self.key);
            if let Some(count) = state.by_extension.get_mut(&self.key.extension_id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.by_extension.remove(&self.key.extension_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_budget_quarantines_after_the_fixed_window_budget() {
        let now = Instant::now();
        let mut budget = RestartBudget::new(RestartPolicy {
            maximum_failures: 2,
            window: Duration::from_secs(60),
            initial_backoff: Duration::from_millis(10),
            maximum_backoff: Duration::from_secs(1),
        });
        assert!(budget.record_failure(now).is_some());
        assert!(budget.record_failure(now).is_some());
        assert!(budget.record_failure(now).is_none());
        assert!(budget
            .record_failure(now + Duration::from_secs(61))
            .is_some());
    }

    #[test]
    fn generation_permits_release_every_counter_on_drop() {
        let limiter = GenerationLimiter::new(GenerationLimits {
            per_extension_tenants: 1,
            per_instance: 2,
            per_extension_tenant: 2,
        });
        let key = GenerationKey {
            extension_id: ExtensionId::new("acme/example").unwrap(),
            tenant_id: 1,
        };
        let active = limiter.acquire(key.clone()).unwrap();
        let candidate = limiter.acquire(key.clone()).unwrap();
        assert!(matches!(
            limiter.acquire(key),
            Err(GenerationAdmissionError::InstanceLimit)
        ));
        drop(active);
        drop(candidate);
        assert_eq!(limiter.active(), 0);
    }
}
