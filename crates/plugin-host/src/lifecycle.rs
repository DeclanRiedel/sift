use std::{
    collections::{HashMap, VecDeque},
    ops::Deref,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sift_extension_protocol::ExtensionId;
use thiserror::Error;
use tokio::sync::Notify;

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

/// Coordinates the active, upgrade-candidate, and rollback generations for
/// one extension/tenant pair.
///
/// The mutex is held only while changing pointers or counters. Work executes
/// through an [`ActiveGenerationLease`] without holding the coordinator lock,
/// so a slow extension cannot serialize unrelated host work.
pub struct GenerationCoordinator<T> {
    state: Arc<Mutex<GenerationState<T>>>,
    drained: Arc<Notify>,
}

struct GenerationState<T> {
    active: Option<Arc<T>>,
    candidate: Option<Arc<T>>,
    rollback: Option<Arc<T>>,
    accepting: bool,
    active_work: usize,
}

pub struct ActiveGenerationLease<T> {
    generation: Arc<T>,
    state: Arc<Mutex<GenerationState<T>>>,
    drained: Arc<Notify>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GenerationTransitionError {
    #[error("extension generation is not accepting new work")]
    AdmissionClosed,
    #[error("extension has no active generation")]
    NoActiveGeneration,
    #[error("extension has no staged upgrade candidate")]
    NoCandidate,
    #[error("extension already has a staged upgrade candidate")]
    CandidateOccupied,
    #[error("extension has no retained rollback generation")]
    NoRollback,
}

pub struct GenerationActivation<T> {
    /// The generation that became active.
    pub active: Arc<T>,
    /// The generation retained for explicit rollback, when one existed.
    pub rollback: Option<Arc<T>>,
    /// Whether all old-generation work completed before the deadline.
    pub drained: bool,
    /// Work still using the old generation when the deadline expired.
    pub remaining_work: usize,
}

impl<T> GenerationCoordinator<T> {
    pub fn new(active: Option<Arc<T>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(GenerationState {
                accepting: active.is_some(),
                active,
                candidate: None,
                rollback: None,
                active_work: 0,
            })),
            drained: Arc::new(Notify::new()),
        }
    }

    pub fn acquire(&self) -> Result<ActiveGenerationLease<T>, GenerationTransitionError> {
        let mut state = self.state.lock().expect("generation state poisoned");
        if !state.accepting {
            return Err(GenerationTransitionError::AdmissionClosed);
        }
        let generation = state
            .active
            .clone()
            .ok_or(GenerationTransitionError::NoActiveGeneration)?;
        state.active_work = state
            .active_work
            .checked_add(1)
            .expect("active generation work counter overflow");
        Ok(ActiveGenerationLease {
            generation,
            state: self.state.clone(),
            drained: self.drained.clone(),
        })
    }

    pub fn stage_candidate(&self, candidate: Arc<T>) -> Result<(), GenerationTransitionError> {
        let mut state = self.state.lock().expect("generation state poisoned");
        if state.candidate.is_some() {
            return Err(GenerationTransitionError::CandidateOccupied);
        }
        state.candidate = Some(candidate);
        Ok(())
    }

    pub fn discard_candidate(&self) -> Option<Arc<T>> {
        self.state
            .lock()
            .expect("generation state poisoned")
            .candidate
            .take()
    }

    /// Blocks new admission, waits up to `deadline` for active work, then
    /// atomically promotes the candidate. A timed-out drain still promotes so
    /// the caller can cancel/close the returned rollback generation without
    /// allowing new work to reach it.
    pub async fn activate_candidate(
        &self,
        deadline: Duration,
    ) -> Result<GenerationActivation<T>, GenerationTransitionError> {
        {
            let mut state = self.state.lock().expect("generation state poisoned");
            if state.candidate.is_none() {
                return Err(GenerationTransitionError::NoCandidate);
            }
            state.accepting = false;
        }
        let drained = self.wait_for_drain(deadline).await;
        let mut state = self.state.lock().expect("generation state poisoned");
        let remaining_work = state.active_work;
        let active = state
            .candidate
            .take()
            .ok_or(GenerationTransitionError::NoCandidate)?;
        let rollback = state.active.replace(active.clone());
        state.rollback = rollback.clone();
        state.accepting = true;
        Ok(GenerationActivation {
            active,
            rollback,
            drained,
            remaining_work,
        })
    }

    /// Promotes the retained generation using the same bounded-drain rules as
    /// a forward activation.
    pub async fn rollback(
        &self,
        deadline: Duration,
    ) -> Result<GenerationActivation<T>, GenerationTransitionError> {
        {
            let mut state = self.state.lock().expect("generation state poisoned");
            if state.rollback.is_none() {
                return Err(GenerationTransitionError::NoRollback);
            }
            state.accepting = false;
        }
        let drained = self.wait_for_drain(deadline).await;
        let mut state = self.state.lock().expect("generation state poisoned");
        let remaining_work = state.active_work;
        let active = state
            .rollback
            .take()
            .ok_or(GenerationTransitionError::NoRollback)?;
        let rollback = state.active.replace(active.clone());
        state.rollback = rollback.clone();
        state.accepting = true;
        Ok(GenerationActivation {
            active,
            rollback,
            drained,
            remaining_work,
        })
    }

    pub fn close_admission(&self) {
        self.state
            .lock()
            .expect("generation state poisoned")
            .accepting = false;
    }

    async fn wait_for_drain(&self, deadline: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.drained.notified();
                if self
                    .state
                    .lock()
                    .expect("generation state poisoned")
                    .active_work
                    == 0
                {
                    break;
                }
                notified.await;
            }
        };
        tokio::time::timeout(deadline, wait).await.is_ok()
    }
}

impl<T> Clone for GenerationCoordinator<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            drained: self.drained.clone(),
        }
    }
}

impl<T> Deref for ActiveGenerationLease<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.generation
    }
}

impl<T> ActiveGenerationLease<T> {
    pub fn generation(&self) -> &Arc<T> {
        &self.generation
    }
}

impl<T> Drop for ActiveGenerationLease<T> {
    fn drop(&mut self) {
        let notify = {
            let mut state = self.state.lock().expect("generation state poisoned");
            state.active_work = state.active_work.saturating_sub(1);
            state.active_work == 0
        };
        if notify {
            self.drained.notify_waiters();
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

    #[tokio::test]
    async fn activation_blocks_admission_drains_and_retains_rollback() {
        let coordinator = GenerationCoordinator::new(Some(Arc::new("v1")));
        let lease = coordinator.acquire().unwrap();
        coordinator.stage_candidate(Arc::new("v2")).unwrap();
        let upgrading = coordinator.clone();
        let activation =
            tokio::spawn(async move { upgrading.activate_candidate(Duration::from_secs(1)).await });
        tokio::task::yield_now().await;
        assert!(matches!(
            coordinator.acquire(),
            Err(GenerationTransitionError::AdmissionClosed)
        ));
        drop(lease);
        let activated = activation.await.unwrap().unwrap();
        assert!(activated.drained);
        assert_eq!(*activated.active, "v2");
        assert_eq!(activated.rollback.as_deref(), Some(&"v1"));
        assert_eq!(activated.remaining_work, 0);

        let rolled_back = coordinator
            .rollback(Duration::from_millis(10))
            .await
            .unwrap();
        assert_eq!(*rolled_back.active, "v1");
        assert_eq!(rolled_back.rollback.as_deref(), Some(&"v2"));
    }

    #[tokio::test]
    async fn activation_deadline_reports_work_that_must_be_cancelled() {
        let coordinator = GenerationCoordinator::new(Some(Arc::new(1)));
        let _lease = coordinator.acquire().unwrap();
        coordinator.stage_candidate(Arc::new(2)).unwrap();
        let activated = coordinator
            .activate_candidate(Duration::from_millis(1))
            .await
            .unwrap();
        assert!(!activated.drained);
        assert_eq!(activated.remaining_work, 1);
        assert_eq!(*coordinator.acquire().unwrap(), 2);
    }
}
