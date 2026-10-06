//! Bound setup work independently of the HTTP waiter: file writes must finish.
use super::*;
use std::future::Future;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

static SETUP_SLOTS: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(8)));

pub(super) fn admit(state: &AppState) -> ApiResult<OwnedSemaphorePermit> {
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
    SETUP_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::RateLimited {
            retry_after_secs: 1,
        })
}

/// Dropping/timing out the waiter detaches this owned task, never its writes.
/// Admission and shutdown accounting stay with the task through settlement.
pub(super) async fn persist<T, F, Fut>(
    state: AppState,
    headers: HeaderMap,
    actor: PrincipalId,
    tenant: TenantId,
    action: String,
    permit: OwnedSemaphorePermit,
    operation: F,
) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce(AuthContext) -> Fut + Send + 'static,
    Fut: Future<Output = ApiResult<T>> + Send + 'static,
{
    let guard = state.shutdown.track_query();
    let task = tokio::spawn(async move {
        let _permit = permit;
        let _guard = guard;
        let result = async {
            let fresh = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
            if fresh.principal_id != actor {
                return Err(ApiError::Unauthorized);
            }
            ensure_tenant(&fresh, tenant)?;
            operation(fresh).await
        }
        .await;
        state.sessions.push_operation_full(
            Operation::Ai {
                action,
                chat_id: None,
                run_id: None,
            },
            if result.is_ok() {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
            Some(actor.0),
            None,
            None,
            result.is_err().then(|| "AI source setup failed".into()),
        );
        if result.is_ok() {
            let fresh = resolve_auth_context_blocking(state.clone(), headers).await?;
            if fresh.principal_id != actor {
                return Err(ApiError::Unauthorized);
            }
            ensure_tenant(&fresh, tenant)?;
        }
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), task)
        .await
        .map_err(|_| {
            ApiError::Conflict(
                "Source setup is still settling. Refresh sources before retrying.".into(),
            )
        })?
        .map_err(|_| ApiError::Internal("AI source setup did not settle".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn disconnected_waiter_does_not_abort_persistence_or_its_audit() {
        let metadata =
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::new()))
                .unwrap();
        metadata.bootstrap_local("setup fixture").unwrap();
        let state = AppState {
            sessions: SessionStore::new(crate::registry::DriverRegistry::builder().build()),
            rooms: RoomRuntime::default(),
            metadata: Some(metadata),
            shutdown: crate::shutdown::Shutdown::default(),
            auth: AuthState {
                loopback_bypass: true,
                ..Default::default()
            },
        };
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_state = state.clone();
        let (task_entered, task_resume, task_committed) =
            (entered.clone(), resume.clone(), committed.clone());
        let permit = admit(&state).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(PEER_ADDR_HEADER, HeaderValue::from_static("127.0.0.1"));
        let waiter = tokio::spawn(async move {
            persist(
                task_state,
                headers,
                PrincipalId(1),
                TenantId(1),
                "setup_fixture".into(),
                permit,
                move |_| async move {
                    task_entered.notify_one();
                    task_resume.notified().await;
                    task_committed.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert_eq!(state.shutdown.in_flight(), 1);
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(!committed.load(std::sync::atomic::Ordering::SeqCst));
        resume.notify_one();
        assert_eq!(
            state
                .shutdown
                .await_drain(std::time::Duration::from_secs(5))
                .await,
            0
        );
        assert!(committed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(state.sessions.list_operations().iter().any(|entry| matches!(&entry.operation, Operation::Ai { action, .. } if action == "setup_fixture") && entry.status == OperationStatus::Succeeded));
    }
}
