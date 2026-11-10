//! Captured UI observation and bounded desktop review requests.

#[derive(Clone)]
pub(crate) struct AiEventSender {
    events: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>,
    scope: Option<sift_workspace_ui::AiViewScope>,
}

impl AiEventSender {
    pub(crate) fn new(
        events: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>,
        scope: Option<sift_workspace_ui::AiViewScope>,
    ) -> Self {
        Self { events, scope }
    }

    pub(crate) fn scope(&self) -> Option<&sift_workspace_ui::AiViewScope> {
        self.scope.as_ref()
    }

    pub(crate) fn send(&self, event: sift_workspace_ui::ExecutorEvent) -> Result<(), ()> {
        let event = match &self.scope {
            Some(scope) => sift_workspace_ui::ExecutorEvent::AiScoped {
                scope: scope.clone(),
                event: Box::new(event),
            },
            None => event,
        };
        self.events.send(event).map_err(|_| ())
    }
}

#[cfg(test)]
impl From<tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>> for AiEventSender {
    fn from(events: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::ExecutorEvent>) -> Self {
        Self::new(events, None)
    }
}

pub(crate) use sift_ai_runtime::{remaining, stopped, History, MAX_ANSWER_BYTES};

impl AiEventSender {
    pub(crate) fn provider_events(&self) -> sift_ai_runtime::AiEventSender {
        let desktop = self.clone();
        sift_ai_runtime::AiEventSender::new(move |event| {
            use sift_ai_runtime::Event;
            let event = match event {
                Event::AiToolActivity(activity) => {
                    sift_workspace_ui::ExecutorEvent::AiToolActivity(activity)
                }
                Event::AiTextDelta(text) => sift_workspace_ui::ExecutorEvent::AiTextDelta(text),
                Event::AiMessage { kind, text } => {
                    sift_workspace_ui::ExecutorEvent::AiMessage { kind, text }
                }
                Event::AiExtensionExecutionStatus(summary) => {
                    sift_workspace_ui::ExecutorEvent::AiExtensionExecutionStatus(summary)
                }
                Event::AiQueryExecutionStatus(summary) => {
                    sift_workspace_ui::ExecutorEvent::AiQueryExecutionStatus(summary)
                }
                Event::AiDatabaseExecutionStatus(summary) => {
                    sift_workspace_ui::ExecutorEvent::AiDatabaseExecutionStatus(summary)
                }
                Event::AiSqlExecutionStatus(summary) => {
                    sift_workspace_ui::ExecutorEvent::AiSqlExecutionStatus(summary)
                }
            };
            desktop.send(event).is_ok()
        })
    }
}

/// Bound pending read/review requests without occupying the desktop command loop.
pub(crate) fn spawn_read<T: Send + 'static>(
    tasks: &mut tokio::task::JoinSet<()>,
    events: AiEventSender,
    read: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    event: impl FnOnce(Result<T, String>) -> sift_workspace_ui::ExecutorEvent + Send + 'static,
) {
    if tasks.len() >= 16 {
        let _ = events.send(event(Err(
            "AI review requests are busy; retry after the pending review finishes".into(),
        )));
        return;
    }
    tasks.spawn(async move {
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), read)
            .await
            .unwrap_or_else(|_| Err("AI review request timed out; retry it".into()));
        let _ = events.send(event(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_events_preserve_captured_view_and_report_a_closed_observer() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let original = sift_workspace_ui::AiViewScope {
            instance_id: "original-server".into(),
            tenant_id: Some(7),
            view_id: uuid::Uuid::new_v4(),
        };
        let desktop = AiEventSender::new(sender, Some(original.clone()));
        let provider = desktop.provider_events();
        let private = "private-provider-text-not-for-debug-logs";
        let event = sift_ai_runtime::Event::AiMessage {
            kind: sift_protocol::AiEventKind::MessageDelta,
            text: private.into(),
        };
        assert!(!format!("{event:?}").contains(private));
        assert!(provider.send(event));
        let sift_workspace_ui::ExecutorEvent::AiScoped { scope, event } =
            receiver.try_recv().unwrap()
        else {
            panic!("captured UI scope required")
        };
        assert_eq!(scope, original);
        assert!(
            matches!(*event,sift_workspace_ui::ExecutorEvent::AiMessage {kind:sift_protocol::AiEventKind::MessageDelta,text} if text==private)
        );
        drop(receiver);
        assert!(!provider.send(sift_ai_runtime::Event::AiTextDelta("late".into())));
    }
}
