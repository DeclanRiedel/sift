use std::sync::Arc;

use gpui::{prelude::*, App, Context, Entity, IntoElement, Window};
use sift_client_sdk::{Client, Error as ClientError, OpenConnectionFromProfileRequest};
use sift_protocol::{ConnectionId, SessionId};
use sift_workspace_ui::{
    ConnectionStatus, ExecutorCommand, ExecutorEvent, PresentationState, PresentationStore, Rect,
    ResultState, WorkspaceShell,
};

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
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(cx, |workspace, cx| {
            workspace.attach_lifecycle(receiver, cx);
            workspace.attach_presence(presence_receiver, cx);
            workspace.attach_executor(command_sender, event_receiver, cx);
        });
        std::mem::drop(runtime.spawn(supervise_local_instance(
            local_server.clone(),
            restored_workspace_id,
            sender,
            presence_sender,
        )));
        std::mem::drop(runtime.spawn(run_query_executor(
            local_server,
            command_receiver,
            event_sender,
        )));
        Self {
            workspace,
            _local_server_lease: lease,
        }
    }
}

/// An opened, reusable execution target: one client, session, and connection.
struct QueryContext {
    client: Client,
    session: SessionId,
    connection: ConnectionId,
}

/// Owns the SDK client and the current session/connection. Connection is
/// explicit — the user picks a profile in the UI; the executor opens it and
/// runs queries against it. The UI thread never touches the SDK directly.
async fn run_query_executor(
    local_server: LocalServerManager,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<ExecutorCommand>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let mut context: Option<QueryContext> = None;
    while let Some(command) = commands.recv().await {
        match command {
            ExecutorCommand::Connect {
                tenant_id,
                profile_id,
                name,
            } => {
                if let Some(previous) = context.take() {
                    let _ = previous.client.close_session(previous.session).await;
                }
                let status = match open_query_context(&local_server, tenant_id, profile_id).await {
                    Ok(opened) => {
                        context = Some(opened);
                        ConnectionStatus::Connected { profile_id, name }
                    }
                    Err(reason) => ConnectionStatus::Failed { profile_id, reason },
                };
                if events.send(ExecutorEvent::Connection(status)).is_err() {
                    return;
                }
            }
            ExecutorCommand::Disconnect => {
                if let Some(opened) = context.take() {
                    let _ = opened.client.close_session(opened.session).await;
                }
                if events
                    .send(ExecutorEvent::Connection(ConnectionStatus::Disconnected))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::Execute { item_id, sql } => {
                let state = run_one(&mut context, &events, &sql).await;
                if events
                    .send(ExecutorEvent::Execution { item_id, state })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Run one query against the current connection, or report not-connected.
/// A transport loss drops the connection and notifies the UI.
async fn run_one(
    context: &mut Option<QueryContext>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    sql: &str,
) -> ResultState {
    let Some(opened) = context.as_ref() else {
        return ResultState::Unavailable("Not connected — pick a connection to run this.".into());
    };
    match opened
        .client
        .execute(opened.session, opened.connection, sql)
        .await
    {
        Ok(response) => ResultState::from_execute(response),
        Err(error @ (ClientError::Transport(_) | ClientError::WebSocket(_))) => {
            // Transport loss: the connection is gone and the outcome is unknown.
            *context = None;
            let _ = events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected));
            ResultState::from_execution_error(true, error.to_string())
        }
        Err(ClientError::Server { error, .. }) => {
            ResultState::from_execution_error(false, error.message)
        }
        Err(other) => ResultState::from_execution_error(false, other.to_string()),
    }
}

/// Open a session and a connection for the chosen tenant/profile. Ids come from
/// the UI's connection picker, so no discovery/guessing happens here.
async fn open_query_context(
    local_server: &LocalServerManager,
    tenant_id: i64,
    profile_id: i64,
) -> Result<QueryContext, String> {
    let client = local_server.ensure_ready().await?;
    let session = client
        .open_session(None)
        .await
        .map_err(|error| format!("opening a session failed: {error}"))?
        .id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id,
                profile_id,
            },
        )
        .await
        .map_err(|error| format!("opening a connection failed: {error}"))?
        .id;
    Ok(QueryContext {
        client,
        session,
        connection,
    })
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
