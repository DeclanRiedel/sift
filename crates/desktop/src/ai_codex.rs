//! Desktop adapter to the shared local isolated provider runtime.
pub(crate) fn configured_model() -> Option<String> {
    sift_ai_runtime::configured_model(sift_protocol::AiProvider::Codex)
}
pub(crate) async fn run(
    client: sift_client_sdk::Client,
    lease: sift_protocol::AiRunLease,
    prompt: String,
    context: sift_protocol::AiTurnContext,
    history: crate::ai_harness::History,
    events: crate::ai_harness::AiEventSender,
) -> Result<(), String> {
    sift_ai_runtime::run(
        client,
        lease,
        prompt,
        context,
        history,
        events.provider_events(),
    )
    .await
}
