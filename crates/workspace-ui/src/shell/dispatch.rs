//! Nonblocking, bounded UI command admission with a coalesced failure notice.

use super::ExecutorCommand;
use tokio::sync::{mpsc, watch};

#[derive(Clone)]
pub struct ExecutorSender {
    sender: mpsc::Sender<ExecutorCommand>,
    failures: watch::Sender<Option<&'static str>>,
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
        (Self { sender, failures }, receiver)
    }

    /// Accepted commands stay ordered. Rejected commands never execute; even
    /// callers that ignore this result produce a visible admission failure.
    pub fn send(&self, command: ExecutorCommand) -> Result<(), CommandSendError> {
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

    pub(super) fn failures(&self) -> watch::Receiver<Option<&'static str>> {
        self.failures.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
