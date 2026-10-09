use super::*;
use sift_workspace_ui::{AccountScope, GithubOwnerSetupEvent};

struct DeviceAttemptGuard {
    client: sift_client_sdk::Client,
    handoff: Option<String>,
}

impl Drop for DeviceAttemptGuard {
    fn drop(&mut self) {
        if let Some(handoff) = self.handoff.take() {
            let client = self.client.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    client.github_owner_device_cancel(handoff),
                )
                .await;
            });
        }
    }
}

pub(super) async fn link_owner(
    server: DesktopServer,
    scope: &AccountScope,
    events: &tokio::sync::mpsc::UnboundedSender<InstanceManagerEvent>,
) -> Result<sift_protocol::WhoAmIResponse, String> {
    if server.instance().id != scope.instance_id {
        return Err("The selected instance changed".into());
    }
    let client = server.client().await?;
    let identity = client.whoami().await.map_err(|e| e.to_string())?;
    if Some(identity.principal.id) != scope.principal_id {
        return Err("The instance owner changed".into());
    }
    let start = client
        .github_owner_device_start()
        .await
        .map_err(|e| e.to_string())?;
    let mut guard = DeviceAttemptGuard {
        client: client.clone(),
        handoff: Some(start.handoff_token.clone()),
    };
    events
        .send(InstanceManagerEvent::GithubOwnerSetup {
            scope: scope.clone(),
            event: GithubOwnerSetupEvent::Authorization {
                url: start.verification_uri,
                code: sift_protocol::RedactedString(start.user_code),
            },
        })
        .map_err(|_| "Desktop closed during GitHub owner setup".to_string())?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(start.expires_in);
    let mut interval = start.interval_secs;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(interval.max(1))).await;
        if tokio::time::Instant::now() >= deadline {
            return Err("GitHub owner setup expired; start again".into());
        }
        let poll = client
            .github_owner_device_poll(start.handoff_token.clone())
            .await
            .map_err(|e| e.to_string())?;
        if poll.completed {
            guard.handoff = None;
            return client.whoami().await.map_err(|e| e.to_string());
        }
        interval = poll.interval_secs;
    }
}
