//! Transfers endpoints on the shared reference client.

use super::*;

impl Client {
    pub async fn transfer_recipes(&self, workspace: WorkspaceId) -> Result<Vec<TransferRecipe>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/transfer-recipes",
            workspace.0
        ))
        .await
    }

    pub async fn create_transfer_recipe(
        &self,
        workspace: WorkspaceId,
        request: CreateTransferRecipeRequest,
    ) -> Result<TransferRecipe> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/transfer-recipes", workspace.0),
            &request,
        )
        .await
    }

    pub async fn transfer_recipe(&self, recipe: TransferRecipeId) -> Result<TransferRecipe> {
        self.get(&format!("/v1/metadata/transfer-recipes/{}", recipe.0))
            .await
    }

    pub async fn update_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
        request: UpdateTransferRecipeRequest,
    ) -> Result<TransferRecipe> {
        self.put(
            &format!("/v1/metadata/transfer-recipes/{}", recipe.0),
            &request,
        )
        .await
    }

    pub async fn delete_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
        request: ExpectedTransferRecipeRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/transfer-recipes/{}", recipe.0),
            &request,
        )
        .await
    }

    pub async fn validate_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
    ) -> Result<TransferRecipe> {
        self.post_empty(&format!(
            "/v1/metadata/transfer-recipes/{}/validate",
            recipe.0
        ))
        .await
    }

    pub async fn execute_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
        request: ExecuteTransferRecipeRequest,
    ) -> Result<TransferExecutionResult> {
        self.post(
            &format!("/v1/metadata/transfer-recipes/{}/execute", recipe.0),
            &request,
        )
        .await
    }
}
