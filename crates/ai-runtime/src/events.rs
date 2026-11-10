//! Observation of provider work conveys no execution or approval authority.
use sift_protocol::*;
use std::sync::Arc;

#[derive(Clone)]
pub enum Event {
    AiToolActivity(String),
    AiTextDelta(String),
    AiMessage { kind: AiEventKind, text: String },
    AiExtensionExecutionStatus(AiExtensionExecutionSummary),
    AiQueryExecutionStatus(AiQueryExecutionSummary),
    AiDatabaseExecutionStatus(AiDatabaseExecutionSummary),
    AiSqlExecutionStatus(AiSqlExecutionSummary),
}

// Private provider text and result-bearing fields must not enter debug logs.
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AiToolActivity(text) => f
                .debug_struct("AiToolActivity")
                .field("bytes", &text.len())
                .finish_non_exhaustive(),
            Self::AiTextDelta(text) => f
                .debug_struct("AiTextDelta")
                .field("bytes", &text.len())
                .finish_non_exhaustive(),
            Self::AiMessage { kind, text } => f
                .debug_struct("AiMessage")
                .field("kind", kind)
                .field("bytes", &text.len())
                .finish_non_exhaustive(),
            Self::AiExtensionExecutionStatus(summary) => f
                .debug_struct("AiExtensionExecutionStatus")
                .field("id", &summary.id)
                .field("state", &summary.state)
                .finish_non_exhaustive(),
            Self::AiQueryExecutionStatus(summary) => f
                .debug_struct("AiQueryExecutionStatus")
                .field("id", &summary.id)
                .field("state", &summary.state)
                .finish_non_exhaustive(),
            Self::AiDatabaseExecutionStatus(summary) => f
                .debug_struct("AiDatabaseExecutionStatus")
                .field("id", &summary.id)
                .field("state", &summary.state)
                .finish_non_exhaustive(),
            Self::AiSqlExecutionStatus(summary) => f
                .debug_struct("AiSqlExecutionStatus")
                .field("id", &summary.id)
                .field("state", &summary.state)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone)]
pub struct AiEventSender {
    sink: Arc<dyn Fn(Event) -> bool + Send + Sync>,
}

impl AiEventSender {
    pub fn new(sink: impl Fn(Event) -> bool + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }
    pub fn send(&self, event: Event) -> bool {
        (self.sink)(event)
    }
}

impl From<tokio::sync::mpsc::UnboundedSender<Event>> for AiEventSender {
    fn from(sender: tokio::sync::mpsc::UnboundedSender<Event>) -> Self {
        Self::new(move |event| sender.send(event).is_ok())
    }
}
