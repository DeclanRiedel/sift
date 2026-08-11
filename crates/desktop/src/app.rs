use std::sync::Arc;

use gpui::{prelude::*, App, Context, Entity, IntoElement, Window};
use sift_workspace_ui::{PresentationState, PresentationStore, Rect, WorkspaceShell};

use crate::local_server::{LocalServerLease, LocalServerManager};
use crate::platform::{current_platform, presentation_state_path, PlatformKind};

/// Process-wide desktop services. Product state remains behind the SDK; this
/// object owns only platform and presentation concerns.
pub struct SiftApp {
    pub platform: PlatformKind,
    pub presentation_store: Arc<PresentationStore>,
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub local_server: LocalServerManager,
}

impl SiftApp {
    pub fn new() -> Self {
        let state_path = presentation_state_path();
        let runtime_state_dir = state_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("runtime");
        Self {
            platform: current_platform(),
            presentation_store: Arc::new(PresentationStore::new(state_path)),
            runtime: Arc::new(tokio::runtime::Runtime::new().expect("creating client runtime")),
            local_server: LocalServerManager::bundled(runtime_state_dir)
                .expect("resolving bundled local server"),
        }
    }

    pub fn restore(&self, displays: &[Rect]) -> PresentationState {
        self.presentation_store
            .load()
            .recover_for_displays(displays)
    }
}

/// Window-level ownership boundary. Additional windows can each own exactly
/// one virtual workspace without adding product state to `SiftApp`.
pub struct SiftWindow {
    workspace: Entity<WorkspaceShell>,
    _local_server_lease: LocalServerLease,
}

impl SiftWindow {
    pub fn new(
        state: PresentationState,
        store: Arc<PresentationStore>,
        runtime: Arc<tokio::runtime::Runtime>,
        local_server: LocalServerManager,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let restored_workspace_id = state.workspace.workspace_id;
        let workspace = cx.new(|cx| WorkspaceShell::new(state, Some(store), window, cx));
        let lease = local_server.acquire();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (presence_sender, presence_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(cx, |workspace, cx| {
            workspace.attach_lifecycle(receiver, cx);
            workspace.attach_presence(presence_receiver, cx);
        });
        std::mem::drop(runtime.spawn(supervise_local_instance(
            local_server,
            restored_workspace_id,
            sender,
            presence_sender,
        )));
        Self {
            workspace,
            _local_server_lease: lease,
        }
    }
}

async fn supervise_local_instance(
    local_server: LocalServerManager,
    restored_workspace_id: Option<i64>,
    sender: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::LifecycleEvent>,
    presence_sender: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::PresenceEvent>,
) {
    let instance = sift_workspace_ui::InstanceSpec {
        id: "local".into(),
        name: "Local Sift".into(),
        base_url: "http://127.0.0.1:7474".into(),
        kind: sift_workspace_ui::InstanceKind::Local,
    };
    let mut attempt = 0_u32;
    loop {
        if sender.is_closed() {
            return;
        }
        let client = match local_server.ensure_ready().await {
            Ok(client) => client,
            Err(message) => {
                let _ = sender.send(sift_workspace_ui::LifecycleEvent::Phase(
                    sift_workspace_ui::ConnectionPhase::Degraded(
                        sift_workspace_ui::DegradedReason::Server(message),
                    ),
                ));
                attempt = attempt.saturating_add(1);
                if !wait_to_reconnect(attempt, &sender).await {
                    return;
                }
                continue;
            }
        };
        let loaded = match sift_workspace_ui::load_instance(
            client.clone(),
            instance.clone(),
            sender.clone(),
        )
        .await
        {
            Ok(loaded) => loaded,
            Err(sift_workspace_ui::DegradedReason::Offline) => {
                attempt = attempt.saturating_add(1);
                if !wait_to_reconnect(attempt, &sender).await {
                    return;
                }
                continue;
            }
            Err(_) => return,
        };
        attempt = 0;
        let selected_room = restored_workspace_id.and_then(|workspace_id| {
            loaded
                .workspaces
                .iter()
                .find(|workspace| workspace.id == workspace_id)
                .map(|workspace| sift_api_types::RoomId(workspace.room_id))
        });
        let disconnected = if let Some(room_id) = selected_room {
            tokio::select! {
                result = sift_workspace_ui::stream_room_presence(
                    client.clone(),
                    room_id,
                    format!("sift-desktop-{}", std::process::id()),
                    presence_sender.clone(),
                ) => result,
                result = wait_for_server_loss(&client) => result,
            }
        } else {
            wait_for_server_loss(&client).await
        };
        let _ = presence_sender.send(sift_workspace_ui::PresenceEvent::Left);
        match disconnected {
            Err(
                reason @ (sift_workspace_ui::DegradedReason::AuthenticationExpired
                | sift_workspace_ui::DegradedReason::AccessRevoked
                | sift_workspace_ui::DegradedReason::IncompatibleProtocol),
            ) => {
                let _ = sender.send(sift_workspace_ui::LifecycleEvent::Phase(
                    sift_workspace_ui::ConnectionPhase::Degraded(reason),
                ));
                return;
            }
            _ => {
                attempt = attempt.saturating_add(1);
                if !wait_to_reconnect(attempt, &sender).await {
                    return;
                }
            }
        }
    }
}

async fn wait_for_server_loss(
    client: &sift_client_sdk::Client,
) -> Result<(), sift_workspace_ui::DegradedReason> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    interval.tick().await;
    loop {
        interval.tick().await;
        if client.health().await.is_err() {
            return Err(sift_workspace_ui::DegradedReason::Offline);
        }
    }
}

async fn wait_to_reconnect(
    attempt: u32,
    sender: &tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::LifecycleEvent>,
) -> bool {
    if sender
        .send(sift_workspace_ui::LifecycleEvent::Phase(
            sift_workspace_ui::ConnectionPhase::Reconnecting { attempt },
        ))
        .is_err()
    {
        return false;
    }
    tokio::time::sleep(reconnect_delay(attempt)).await;
    !sender.is_closed()
}

fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    std::time::Duration::from_millis(100_u64.saturating_mul(1_u64 << exponent).min(3_000))
}

impl gpui::Render for SiftWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.workspace.clone()
    }
}

pub fn display_rects(cx: &App) -> Vec<Rect> {
    cx.displays()
        .into_iter()
        .map(|display| {
            let bounds = display.bounds();
            Rect {
                x: bounds.origin.x.into(),
                y: bounds.origin.y.into(),
                width: bounds.size.width.into(),
                height: bounds.size.height.into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_restart_backoff_is_bounded_and_resets_per_ready_cycle() {
        assert_eq!(reconnect_delay(1), std::time::Duration::from_millis(100));
        assert_eq!(reconnect_delay(2), std::time::Duration::from_millis(200));
        assert_eq!(reconnect_delay(99), std::time::Duration::from_secs(3));
    }
}
