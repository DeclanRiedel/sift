//! Desktop adapter to the shared local isolated provider runtime.
pub(crate) fn configured_model(provider: sift_protocol::AiProvider) -> Option<String> {
    sift_ai_runtime::configured_model(provider)
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
