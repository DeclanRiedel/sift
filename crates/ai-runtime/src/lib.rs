//! Local isolated provider execution and Sift-only tools, independent of a UI.
mod ai_cli;
mod ai_codex;
mod ai_harness;
mod ai_mcp_bridge;
mod ai_models;
mod ai_tools;
mod events;

pub use ai_harness::{remaining, stopped, History, MAX_ANSWER_BYTES};
pub use events::{AiEventSender, Event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub reasoning: Vec<String>,
    pub default_reasoning: Option<String>,
    pub is_default: bool,
}

pub async fn load_models(provider: sift_protocol::AiProvider) -> Result<Vec<ModelOption>, String> {
    ai_models::load(provider).await
}

pub fn configured_model(provider: sift_protocol::AiProvider) -> Option<String> {
    match provider {
        sift_protocol::AiProvider::Codex => ai_codex::configured_model(),
        provider => ai_cli::configured_model(provider),
    }
}

/// Execute one already admitted provider turn.
///
/// The host retains Stop, the absolute run deadline and terminal Sift status
/// ownership. The client retains the original queued caller for every tool and
/// receipt; this boundary cannot accept a mutable ordinary SDK client.
/// Observing events grants no approval or delegation authority.
pub async fn run(
    client: sift_client_sdk::ScopedAiClient,
    lease: sift_protocol::AiRunLease,
    prompt: String,
    context: sift_protocol::AiTurnContext,
    history: History,
    events: AiEventSender,
) -> Result<(), String> {
    match lease.run.provider {
        sift_protocol::AiProvider::Codex => {
            ai_codex::run(client, lease, prompt, context, history, events).await
        }
        sift_protocol::AiProvider::ClaudeCode | sift_protocol::AiProvider::OpenCode => {
            ai_cli::run(client, lease, prompt, context, history, events).await
        }
    }
}
