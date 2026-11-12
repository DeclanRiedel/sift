pub(crate) async fn load(
    provider: sift_protocol::AiProvider,
) -> Result<Vec<sift_workspace_ui::AiModelOption>, String> {
    Ok(sift_ai_runtime::load_models(provider)
        .await?
        .into_iter()
        .map(|model| sift_workspace_ui::AiModelOption {
            id: model.id,
            name: model.name,
            reasoning: model.reasoning,
            default_reasoning: model.default_reasoning,
            is_default: model.is_default,
        })
        .collect())
}
