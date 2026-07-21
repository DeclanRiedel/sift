//! Per-tenant live resource admission with RAII release (ADR-020).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sift_metadata::{MetadataStore, TenantId};
use sift_protocol::{
    TenantResource, TenantResourceLimits, TenantResourceUsage, TenantUsageSnapshot,
};

use crate::config::TenantLimitsConfig;
use crate::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct ResourceManager {
    inner: Arc<Inner>,
}

struct Inner {
    usage: Mutex<HashMap<TenantId, TenantResourceUsage>>,
    defaults: TenantResourceLimits,
    ceilings: TenantResourceLimits,
    metadata: Option<MetadataStore>,
    trusted_local_unlimited: bool,
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                usage: Mutex::new(HashMap::new()),
                defaults: TenantResourceLimits::default(),
                ceilings: TenantResourceLimits::default(),
                metadata: None,
                trusted_local_unlimited: true,
            }),
        }
    }
}

impl ResourceManager {
    pub fn new(config: &TenantLimitsConfig, metadata: Option<MetadataStore>) -> Self {
        Self {
            inner: Arc::new(Inner {
                usage: Mutex::new(HashMap::new()),
                defaults: config.defaults.clone(),
                ceilings: config.ceilings.clone(),
                metadata,
                trusted_local_unlimited: config.trusted_local_unlimited,
            }),
        }
    }

    pub fn enforces_for(&self, trusted_local: bool) -> bool {
        !(trusted_local && self.inner.trusted_local_unlimited)
    }

    pub fn reserve(
        &self,
        tenant: TenantId,
        resource: TenantResource,
        amount: u64,
    ) -> ApiResult<ResourceGuard> {
        let limits = self.effective_limits(tenant)?;
        let limit = limit_for(&limits, resource);
        let mut usages = self.inner.usage.lock().unwrap();
        let usage = usages.entry(tenant).or_default();
        let current = usage_for(usage, resource);
        if limit.is_some_and(|limit| current.saturating_add(amount) > limit) {
            return Err(ApiError::TenantResourceExhausted {
                resource,
                retry_after_secs: None,
                durable: false,
            });
        }
        *usage_for_mut(usage, resource) = current.saturating_add(amount);
        Ok(ResourceGuard {
            manager: self.clone(),
            tenant,
            resource,
            amount,
            released: false,
        })
    }

    pub fn effective_limits(&self, tenant: TenantId) -> ApiResult<TenantResourceLimits> {
        let requested = match &self.inner.metadata {
            Some(metadata) => metadata
                .get_tenant_limit_override(tenant)?
                .map(|row| row.limits)
                .unwrap_or_else(|| self.inner.defaults.clone()),
            None => self.inner.defaults.clone(),
        };
        Ok(clamp_limits(&requested, &self.inner.ceilings))
    }

    pub fn snapshot(&self, tenant: TenantId) -> ApiResult<TenantUsageSnapshot> {
        let mut usage = self
            .inner
            .usage
            .lock()
            .unwrap()
            .get(&tenant)
            .cloned()
            .unwrap_or_default();
        if let Some(metadata) = &self.inner.metadata {
            usage.connection_profiles = metadata.list_connection_profiles(tenant)?.len() as u64;
        }
        Ok(TenantUsageSnapshot {
            tenant_id: tenant.0,
            limits: self.effective_limits(tenant)?,
            usage,
        })
    }

    pub fn validate_override(&self, limits: &TenantResourceLimits) -> ApiResult<()> {
        if &clamp_limits(limits, &self.inner.ceilings) == limits {
            Ok(())
        } else {
            Err(ApiError::BadRequest(
                "tenant limit override exceeds an operator ceiling".into(),
            ))
        }
    }

    fn release(&self, tenant: TenantId, resource: TenantResource, amount: u64) {
        let mut usages = self.inner.usage.lock().unwrap();
        let Some(usage) = usages.get_mut(&tenant) else {
            return;
        };
        let value = usage_for_mut(usage, resource);
        *value = value.saturating_sub(amount);
        if usage == &TenantResourceUsage::default() {
            usages.remove(&tenant);
        }
    }
}

pub struct ResourceGuard {
    manager: ResourceManager,
    tenant: TenantId,
    resource: TenantResource,
    amount: u64,
    released: bool,
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        if !self.released {
            self.manager
                .release(self.tenant, self.resource, self.amount);
            self.released = true;
        }
    }
}

fn clamp_limits(
    requested: &TenantResourceLimits,
    ceilings: &TenantResourceLimits,
) -> TenantResourceLimits {
    TenantResourceLimits {
        connection_profiles: clamp(requested.connection_profiles, ceilings.connection_profiles),
        sessions: clamp(requested.sessions, ceilings.sessions),
        connections: clamp(requested.connections, ceilings.connections),
        concurrent_queries: clamp(requested.concurrent_queries, ceilings.concurrent_queries),
        cursors: clamp(requested.cursors, ceilings.cursors),
        retained_result_bytes: clamp(
            requested.retained_result_bytes,
            ceilings.retained_result_bytes,
        ),
    }
}

fn clamp(requested: Option<u64>, ceiling: Option<u64>) -> Option<u64> {
    match (requested, ceiling) {
        (Some(requested), Some(ceiling)) => Some(requested.min(ceiling)),
        (None, Some(ceiling)) => Some(ceiling),
        (requested, None) => requested,
    }
}

fn limit_for(limits: &TenantResourceLimits, resource: TenantResource) -> Option<u64> {
    match resource {
        TenantResource::ConnectionProfiles => limits.connection_profiles,
        TenantResource::Sessions => limits.sessions,
        TenantResource::Connections => limits.connections,
        TenantResource::ConcurrentQueries => limits.concurrent_queries,
        TenantResource::Cursors => limits.cursors,
        TenantResource::RetainedResultBytes => limits.retained_result_bytes,
    }
}

fn usage_for(usage: &TenantResourceUsage, resource: TenantResource) -> u64 {
    match resource {
        TenantResource::ConnectionProfiles => usage.connection_profiles,
        TenantResource::Sessions => usage.sessions,
        TenantResource::Connections => usage.connections,
        TenantResource::ConcurrentQueries => usage.concurrent_queries,
        TenantResource::Cursors => usage.cursors,
        TenantResource::RetainedResultBytes => usage.retained_result_bytes,
    }
}

fn usage_for_mut(usage: &mut TenantResourceUsage, resource: TenantResource) -> &mut u64 {
    match resource {
        TenantResource::ConnectionProfiles => &mut usage.connection_profiles,
        TenantResource::Sessions => &mut usage.sessions,
        TenantResource::Connections => &mut usage.connections,
        TenantResource::ConcurrentQueries => &mut usage.concurrent_queries,
        TenantResource::Cursors => &mut usage.cursors,
        TenantResource::RetainedResultBytes => &mut usage.retained_result_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_are_atomic_and_release_on_drop() {
        let mut config = TenantLimitsConfig::default();
        config.defaults.connections = Some(1);
        config.ceilings.connections = Some(1);
        let manager = ResourceManager::new(&config, None);
        let guard = manager
            .reserve(TenantId(1), TenantResource::Connections, 1)
            .unwrap();
        assert!(manager
            .reserve(TenantId(1), TenantResource::Connections, 1)
            .is_err());
        drop(guard);
        assert!(manager
            .reserve(TenantId(1), TenantResource::Connections, 1)
            .is_ok());
    }

    #[test]
    fn operator_ceiling_bounds_unlimited_override() {
        assert_eq!(clamp(None, Some(5)), Some(5));
        assert_eq!(clamp(Some(8), Some(5)), Some(5));
        assert_eq!(clamp(None, None), None);
    }

    #[test]
    fn concurrent_quota_admission_has_one_winner_and_releases_cleanly() {
        let mut config = TenantLimitsConfig::default();
        config.defaults.concurrent_queries = Some(1);
        config.ceilings.concurrent_queries = Some(1);
        let manager = ResourceManager::new(&config, None);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let manager = manager.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let reservation =
                    manager.reserve(TenantId(1), TenantResource::ConcurrentQueries, 1);
                barrier.wait();
                reservation
            }));
        }
        barrier.wait();
        barrier.wait();
        let reservations: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            reservations.iter().filter(|result| result.is_ok()).count(),
            1
        );
        drop(reservations);
        assert!(manager
            .reserve(TenantId(1), TenantResource::ConcurrentQueries, 1)
            .is_ok());
    }

    #[test]
    fn trusted_local_exemption_is_explicit_and_configurable() {
        let defaults = TenantLimitsConfig::default();
        assert!(!ResourceManager::new(&defaults, None).enforces_for(true));

        let mut constrained = defaults;
        constrained.trusted_local_unlimited = false;
        assert!(ResourceManager::new(&constrained, None).enforces_for(true));
    }
}
