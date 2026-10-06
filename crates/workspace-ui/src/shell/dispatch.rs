//! Nonblocking, bounded UI command admission with a coalesced failure notice.

use super::{AiViewScope, ExecutorCommand};
use tokio::sync::{mpsc, watch};

#[derive(Clone)]
pub struct ExecutorSender {
    sender: mpsc::Sender<ExecutorCommand>,
    failures: watch::Sender<Option<&'static str>>,
    ai_scope: Option<AiViewScope>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommandSendError {
    Full,
    Closed,
}

impl ExecutorSender {
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<ExecutorCommand>) {
        let (sender, receiver) = mpsc::channel(capacity);
        let (failures, _) = watch::channel(None);
        (
            Self {
                sender,
                failures,
                ai_scope: None,
            },
            receiver,
        )
    }

    /// Accepted commands stay ordered. Rejected commands never execute; even
    /// callers that ignore this result produce a visible admission failure.
    pub fn send(&self, command: ExecutorCommand) -> Result<(), CommandSendError> {
        let command = match self.ai_scope.as_ref().filter(|_| {
            matches!(
                command,
                ExecutorCommand::ReviewAiDatabaseProposal { .. }
                    | ExecutorCommand::ApplyAiDatabaseProposal { .. }
                    | ExecutorCommand::DiscardAiDatabaseProposal { .. }
                    | ExecutorCommand::ReviewAiPublication { .. }
                    | ExecutorCommand::ChangeAiPublication { .. }
                    | ExecutorCommand::LoadAiChat { .. }
                    | ExecutorCommand::ListAiRoomResults { .. }
                    | ExecutorCommand::PreviewAiAttachment { .. }
                    | ExecutorCommand::SendAiTurn { .. }
                    | ExecutorCommand::StopAiTurn
                    | ExecutorCommand::MarkAiProposalApplied { .. }
                    | ExecutorCommand::DiscardAiProposal { .. }
            )
        }) {
            Some(scope) => ExecutorCommand::AiScoped {
                scope: scope.clone(),
                command: Box::new(command),
            },
            None => command,
        };
        self.sender.try_send(command).map_err(|error| {
            let (error, message) = match error {
                mpsc::error::TrySendError::Full(_) => (
                    CommandSendError::Full,
                    "Command queue is full. An action was not queued; retry it when Sift is ready.",
                ),
                mpsc::error::TrySendError::Closed(_) => (
                    CommandSendError::Closed,
                    "Command service is unavailable. An action was not queued.",
                ),
            };
            self.failures.send_replace(Some(message));
            error
        })
    }

    pub(super) fn ai_scope(&self) -> Option<&AiViewScope> {
        self.ai_scope.as_ref()
    }

    pub(super) fn with_ai_scope(&self, scope: AiViewScope) -> Self {
        Self {
            ai_scope: Some(scope),
            ..self.clone()
        }
    }

    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    pub(super) fn failures(&self) -> watch::Receiver<Option<&'static str>> {
        self.failures.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captured_ai_senders_keep_their_original_scope() {
        let (sender, mut commands) = ExecutorSender::channel(2);
        let original = AiViewScope {
            instance_id: "first".into(),
            tenant_id: Some(1),
            view_id: uuid::Uuid::new_v4(),
        };
        let next = AiViewScope {
            instance_id: "second".into(),
            tenant_id: Some(2),
            view_id: uuid::Uuid::new_v4(),
        };
        let captured = sender.with_ai_scope(original.clone());
        let current = captured.with_ai_scope(next.clone());
        captured.send(ExecutorCommand::StopAiTurn).unwrap();
        current.send(ExecutorCommand::StopAiTurn).unwrap();
        for expected in [original, next] {
            let Some(ExecutorCommand::AiScoped { scope, command }) = commands.recv().await else {
                panic!("AI command must preserve its scope");
            };
            assert_eq!(scope, expected);
            assert!(matches!(*command, ExecutorCommand::StopAiTurn));
        }
        current.send(ExecutorCommand::LoadSessions).unwrap();
        assert!(matches!(
            commands.recv().await,
            Some(ExecutorCommand::LoadSessions)
        ));
    }

    #[tokio::test]
    async fn rejected_commands_are_reported_and_never_enter_the_queue() {
        let (sender, mut commands) = ExecutorSender::channel(1);
        let mut failures = sender.failures();
        sender.send(ExecutorCommand::LoadSessions).unwrap();
        assert_eq!(
            sender.send(ExecutorCommand::Disconnect),
            Err(CommandSendError::Full)
        );
        failures.changed().await.unwrap();
        assert!(failures.borrow_and_update().unwrap().contains("not queued"));
        assert!(matches!(
            commands.recv().await,
            Some(ExecutorCommand::LoadSessions)
        ));
        assert!(commands.try_recv().is_err());
        sender.send(ExecutorCommand::LoadApiTokens).unwrap();
        assert!(matches!(
            commands.recv().await,
            Some(ExecutorCommand::LoadApiTokens)
        ));
        drop(commands);
        assert_eq!(
            sender.send(ExecutorCommand::Disconnect),
            Err(CommandSendError::Closed)
        );
    }
}
