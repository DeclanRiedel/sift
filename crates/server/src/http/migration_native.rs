//! Native observations have their own versioned read contract; workflow DTOs stay stable.
use super::*;
use sift_protocol::{MigrationNativeOutcomesPage, MigrationNativePageRequest};

pub(super) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant, run)): Path<(i64, String)>,
    Query(request): Query<MigrationNativePageRequest>,
) -> ApiResult<Json<MigrationNativeOutcomesPage>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let tenant = tenant_id(tenant)?;
    let run = migration_run_id(&run)?;
    let actor = auth.principal_id;
    let operation = Operation::GetMigrationNativeOutcomes {
        tenant_id: tenant.0,
        run_id: run,
    };
    super::execution::owned_audited_operation(state.clone(), operation, Some(actor.0), async move {
        ensure_tenant(&auth, tenant)?;
        let metadata = metadata_store_cloned(&state)?;
        let page = metadata_blocking(move || {
            metadata
                .migration_native_outcomes(tenant, run, &request)
                .map_err(Into::into)
        })
        .await?;
        // SQLite admission may wait. Recheck disclosure after that boundary.
        let fresh = resolve_auth_context_blocking(state, headers).await?;
        if fresh.principal_id != actor {
            return Err(ApiError::Unauthorized);
        }
        ensure_tenant(&fresh, tenant)?;
        Ok(Json(page))
    })
    .await
}
