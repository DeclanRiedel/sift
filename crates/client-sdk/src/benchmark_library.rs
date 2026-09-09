use super::*;

impl Client {
    pub async fn save_benchmark_run(
        &self,
        tenant: TenantId,
        request: &sift_protocol::SaveBenchmarkRunRequest,
    ) -> Result<sift_protocol::SavedBenchmarkRun> {
        self.post(
            &format!("/v1/metadata/tenants/{}/benchmark-runs", tenant.0),
            request,
        )
        .await
    }

    pub async fn benchmark_runs(
        &self,
        tenant: TenantId,
        cursor: Option<uuid::Uuid>,
    ) -> Result<CursorPage<sift_protocol::SavedBenchmarkRunSummary>> {
        let query = cursor.map_or_else(|| "limit=50".into(), |id| format!("limit=50&cursor={id}"));
        self.get(&format!(
            "/v1/metadata/tenants/{}/benchmark-runs?{query}",
            tenant.0
        ))
        .await
    }

    pub async fn saved_benchmark_run(
        &self,
        tenant: TenantId,
        id: uuid::Uuid,
    ) -> Result<sift_protocol::SavedBenchmarkRun> {
        self.get(&format!(
            "/v1/metadata/tenants/{}/benchmark-runs/{id}",
            tenant.0
        ))
        .await
    }

    pub async fn delete_benchmark_run(&self, tenant: TenantId, id: uuid::Uuid) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/tenants/{}/benchmark-runs/{id}",
            tenant.0
        ))
        .await
    }
}
