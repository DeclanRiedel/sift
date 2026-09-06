//! Vault endpoints on the shared reference client.

use super::*;

impl Client {
    pub async fn vaults(&self, tenant: TenantId) -> Result<Vec<Vault>> {
        self.get(&format!("/v1/metadata/vaults?tenant={}", tenant.0))
            .await
    }

    pub async fn create_vault(&self, request: CreateVaultRequest) -> Result<Vault> {
        self.post("/v1/metadata/vaults", &request).await
    }

    pub async fn vault(&self, vault: VaultId) -> Result<Vault> {
        self.get(&format!("/v1/metadata/vaults/{}", vault.0)).await
    }

    pub async fn update_vault(
        &self,
        vault: VaultId,
        request: sift_api_types::UpdateVaultRequest,
    ) -> Result<Vault> {
        self.put(&format!("/v1/metadata/vaults/{}", vault.0), &request)
            .await
    }

    pub async fn delete_vault(&self, vault: VaultId, expected_revision: u64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/vaults/{}?expected_revision={expected_revision}",
            vault.0
        ))
        .await
    }

    pub async fn vault_items(&self, vault: VaultId) -> Result<Vec<VaultItem>> {
        self.get(&format!("/v1/metadata/vaults/{}/items", vault.0))
            .await
    }

    pub async fn create_vault_item(
        &self,
        vault: VaultId,
        request: CreateVaultItemRequest,
    ) -> Result<VaultItem> {
        self.post(&format!("/v1/metadata/vaults/{}/items", vault.0), &request)
            .await
    }

    pub async fn vault_item(&self, item: VaultItemId) -> Result<VaultItem> {
        self.get(&format!("/v1/metadata/vault-items/{}", item.0))
            .await
    }

    pub async fn update_vault_item(
        &self,
        item: VaultItemId,
        request: sift_api_types::UpdateVaultItemRequest,
    ) -> Result<VaultItem> {
        self.put(&format!("/v1/metadata/vault-items/{}", item.0), &request)
            .await
    }

    pub async fn delete_vault_item(&self, item: VaultItemId, expected_revision: u64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/vault-items/{}?expected_revision={expected_revision}",
            item.0
        ))
        .await
    }

    pub async fn set_vault_item_secret(
        &self,
        item: VaultItemId,
        request: sift_api_types::SetVaultSecretRequest,
    ) -> Result<VaultItem> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/secret", item.0),
            &request,
        )
        .await
    }

    pub async fn clear_vault_item_secret(
        &self,
        item: VaultItemId,
        expected_revision: u64,
    ) -> Result<VaultItem> {
        self.delete_response(&format!(
            "/v1/metadata/vault-items/{}/secret?expected_revision={expected_revision}",
            item.0
        ))
        .await
    }

    pub async fn vault_grants(&self, vault: VaultId) -> Result<Vec<VaultGrant>> {
        self.get(&format!("/v1/metadata/vaults/{}/grants", vault.0))
            .await
    }

    pub async fn set_vault_grant(
        &self,
        vault: VaultId,
        principal: sift_api_types::PrincipalId,
        request: SetVaultGrantRequest,
    ) -> Result<VaultGrant> {
        self.put(
            &format!("/v1/metadata/vaults/{}/grants/{}", vault.0, principal.0),
            &request,
        )
        .await
    }

    pub async fn delete_vault_grant(
        &self,
        vault: VaultId,
        principal: sift_api_types::PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/vaults/{}/grants/{}?expected_revision={expected_revision}",
            vault.0, principal.0
        ))
        .await
    }

    pub async fn vault_item_versions(&self, item: VaultItemId) -> Result<Vec<VaultItemVersion>> {
        self.get(&format!("/v1/metadata/vault-items/{}/versions", item.0))
            .await
    }

    pub async fn vault_item_version(
        &self,
        item: VaultItemId,
        version: u64,
    ) -> Result<VaultItemVersion> {
        self.get(&format!(
            "/v1/metadata/vault-items/{}/versions/{version}",
            item.0
        ))
        .await
    }

    pub async fn diff_vault_item_versions(
        &self,
        item: VaultItemId,
        from: u64,
        to: u64,
    ) -> Result<sift_api_types::VaultItemVersionDiff> {
        self.get(&format!(
            "/v1/metadata/vault-items/{}/diff?from={from}&to={to}",
            item.0
        ))
        .await
    }

    pub async fn restore_vault_item(
        &self,
        item: VaultItemId,
        request: sift_api_types::RestoreVaultItemRequest,
    ) -> Result<VaultItem> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/restore", item.0),
            &request,
        )
        .await
    }

    pub async fn test_vault_item(&self, item: VaultItemId) -> Result<()> {
        let _: serde_json::Value = self
            .post_empty(&format!("/v1/metadata/vault-items/{}/test", item.0))
            .await?;
        Ok(())
    }

    pub async fn step_up_vault_reveal(
        &self,
        item: VaultItemId,
        request: sift_api_types::VaultRevealStepUpRequest,
    ) -> Result<sift_api_types::VaultRevealStepUpResponse> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/reveal-step-up", item.0),
            &request,
        )
        .await
    }

    pub async fn reveal_vault_item(
        &self,
        item: VaultItemId,
        lease: Option<String>,
    ) -> Result<RevealVaultSecretResponse> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/reveal", item.0),
            &sift_api_types::VaultRevealRequest { lease },
        )
        .await
    }
}
