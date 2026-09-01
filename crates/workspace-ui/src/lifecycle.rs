use std::time::Duration;

use sift_api_types::{DocumentId, RoomId, TenantId};
use sift_client_sdk::{Client, CreateWorkspaceRequest, Error as ClientError};
use sift_protocol::{
    HandshakeResponse, ProviderDescriptor, RoomPresence, RoomServerMessage, WhoAmIResponse,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceKind {
    Local,
    Ssh,
    Hosted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSpec {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub kind: InstanceKind,
}

#[derive(Debug, Clone, Default)]
pub struct InstanceCatalog {
    entries: Vec<InstanceSpec>,
    selected_id: Option<String>,
}

impl InstanceCatalog {
    pub fn new(entries: Vec<InstanceSpec>, restored_id: Option<&str>) -> Self {
        let selected_id = restored_id
            .filter(|id| entries.iter().any(|entry| entry.id == *id))
            .map(str::to_owned)
            .or_else(|| entries.first().map(|entry| entry.id.clone()));
        Self {
            entries,
            selected_id,
        }
    }

    pub fn entries(&self) -> &[InstanceSpec] {
        &self.entries
    }

    pub fn selected(&self) -> Option<&InstanceSpec> {
        let selected = self.selected_id.as_deref()?;
        self.entries.iter().find(|entry| entry.id == selected)
    }

    pub fn select(&mut self, id: &str) -> bool {
        if self.entries.iter().any(|entry| entry.id == id) {
            self.selected_id = Some(id.to_owned());
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegradedReason {
    Offline,
    AuthenticationExpired,
    AccessRevoked,
    IncompatibleProtocol,
    Server(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionPhase {
    Offline,
    Connecting,
    Negotiating,
    Authenticating,
    LoadingNavigation,
    Ready,
    Reconnecting { attempt: u32 },
    Degraded(DegradedReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceNavEntry {
    pub id: i64,
    pub room_id: i64,
    pub name: String,
    pub git_enabled: bool,
    pub scheduling_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomNavEntry {
    pub id: RoomId,
    pub tenant_id: TenantId,
    pub name: String,
    pub workspaces: Vec<WorkspaceNavEntry>,
    pub documents: Vec<DocumentNavEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentNavEntry {
    pub id: DocumentId,
    pub room_id: RoomId,
    pub title: String,
    pub kind: String,
    pub position: i64,
    pub snapshot: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionNavEntry {
    pub id: i64,
    pub tenant_id: i64,
    pub name: String,
    pub provider_id: sift_protocol::ProviderId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantNavEntry {
    pub id: TenantId,
    pub name: String,
    pub rooms: Vec<RoomNavEntry>,
    pub connections: Vec<ConnectionNavEntry>,
}

#[derive(Debug, Clone)]
pub struct LifecycleProjection {
    pub selected_instance: Option<InstanceSpec>,
    pub phase: ConnectionPhase,
    pub handshake: Option<HandshakeResponse>,
    pub identity: Option<WhoAmIResponse>,
    pub providers: Vec<ProviderDescriptor>,
    pub tenants: Vec<TenantNavEntry>,
}

impl Default for LifecycleProjection {
    fn default() -> Self {
        Self {
            selected_instance: None,
            phase: ConnectionPhase::Offline,
            handshake: None,
            identity: None,
            providers: Vec::new(),
            tenants: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    Selected(InstanceSpec),
    Phase(ConnectionPhase),
    Negotiated(HandshakeResponse),
    Authenticated(WhoAmIResponse),
    Providers(Vec<ProviderDescriptor>),
    TenantLoaded(TenantNavEntry),
    ResetNavigation,
}

#[derive(Debug, Clone, Default)]
pub struct LoadedInstance {
    pub workspaces: Vec<WorkspaceNavEntry>,
}

#[derive(Debug, Clone)]
pub enum PresenceEvent {
    Joined {
        room_id: RoomId,
        attachment_id: i64,
        presence: Vec<RoomPresence>,
    },
    Message(RoomServerMessage),
    Left,
}

impl LifecycleProjection {
    pub fn apply(&mut self, event: LifecycleEvent) {
        match event {
            LifecycleEvent::Selected(instance) => {
                self.selected_instance = Some(instance);
                self.handshake = None;
                self.identity = None;
                self.providers.clear();
                self.tenants.clear();
            }
            LifecycleEvent::Phase(phase) => self.phase = phase,
            LifecycleEvent::Negotiated(handshake) => self.handshake = Some(handshake),
            LifecycleEvent::Authenticated(identity) => self.identity = Some(identity),
            LifecycleEvent::Providers(providers) => self.providers = providers,
            LifecycleEvent::TenantLoaded(tenant) => {
                if let Some(existing) = self.tenants.iter_mut().find(|row| row.id == tenant.id) {
                    *existing = tenant;
                } else {
                    self.tenants.push(tenant);
                    self.tenants
                        .sort_by(|left, right| left.name.cmp(&right.name));
                }
            }
            LifecycleEvent::ResetNavigation => self.tenants.clear(),
        }
    }

    pub fn status_label(&self) -> String {
        match &self.phase {
            ConnectionPhase::Offline => "Offline".into(),
            ConnectionPhase::Connecting => "Connecting…".into(),
            ConnectionPhase::Negotiating => "Negotiating protocol…".into(),
            ConnectionPhase::Authenticating => "Authenticating…".into(),
            ConnectionPhase::LoadingNavigation => "Loading workspace…".into(),
            ConnectionPhase::Ready => "Ready".into(),
            ConnectionPhase::Reconnecting { attempt } => format!("Reconnecting ({attempt})…"),
            ConnectionPhase::Degraded(reason) => match reason {
                DegradedReason::Offline => "Offline · retry available".into(),
                DegradedReason::AuthenticationExpired => "Sign in again".into(),
                DegradedReason::AccessRevoked => "Workspace access revoked".into(),
                DegradedReason::IncompatibleProtocol => "Client/server update required".into(),
                DegradedReason::Server(message) => format!("Degraded · {message}"),
            },
        }
    }
}

fn degraded(error: &ClientError) -> DegradedReason {
    match error {
        ClientError::Transport(_) | ClientError::WebSocket(_) => DegradedReason::Offline,
        ClientError::Server { status, .. } => degraded_http_status(status.as_u16())
            .unwrap_or_else(|| DegradedReason::Server(error.to_string())),
        ClientError::Protocol(_) => DegradedReason::IncompatibleProtocol,
        other => DegradedReason::Server(other.to_string()),
    }
}

fn degraded_http_status(status: u16) -> Option<DegradedReason> {
    match status {
        401 => Some(DegradedReason::AuthenticationExpired),
        403 => Some(DegradedReason::AccessRevoked),
        _ => None,
    }
}

fn send(sender: &mpsc::UnboundedSender<LifecycleEvent>, event: LifecycleEvent) -> bool {
    sender.send(event).is_ok()
}

fn fail(sender: &mpsc::UnboundedSender<LifecycleEvent>, error: &ClientError) -> DegradedReason {
    let reason = degraded(error);
    let _ = sender.send(LifecycleEvent::Phase(ConnectionPhase::Degraded(
        reason.clone(),
    )));
    reason
}

/// Load one selected instance progressively through the public SDK. Local,
/// SSH, and hosted instances differ only in how their base URL and auth are
/// obtained; all product calls follow this path.
pub async fn load_instance(
    client: Client,
    instance: InstanceSpec,
    sender: mpsc::UnboundedSender<LifecycleEvent>,
) -> Result<LoadedInstance, DegradedReason> {
    if !send(&sender, LifecycleEvent::Selected(instance))
        || !send(&sender, LifecycleEvent::Phase(ConnectionPhase::Connecting))
    {
        return Err(DegradedReason::Offline);
    }
    if let Err(error) = client.health().await {
        return Err(fail(&sender, &error));
    }
    let readiness = match client.ready().await {
        Ok(readiness) => readiness,
        Err(error) => return Err(fail(&sender, &error)),
    };
    if !readiness.ready {
        let reason = DegradedReason::Server(if readiness.draining {
            "server is draining".into()
        } else if !readiness.drivers_registered {
            "no database providers are ready".into()
        } else if readiness.metadata_ok == Some(false) {
            "metadata is unavailable".into()
        } else {
            "server is not ready".into()
        });
        let _ = sender.send(LifecycleEvent::Phase(ConnectionPhase::Degraded(
            reason.clone(),
        )));
        return Err(reason);
    }
    if !send(&sender, LifecycleEvent::Phase(ConnectionPhase::Negotiating)) {
        return Err(DegradedReason::Offline);
    }
    let handshake = match client.connect().await {
        Ok(handshake) => handshake,
        Err(error) => return Err(fail(&sender, &error)),
    };
    if !send(&sender, LifecycleEvent::Negotiated(handshake))
        || !send(
            &sender,
            LifecycleEvent::Phase(ConnectionPhase::Authenticating),
        )
    {
        return Err(DegradedReason::Offline);
    }
    let identity = match client.whoami().await {
        Ok(identity) => identity,
        Err(error) => return Err(fail(&sender, &error)),
    };
    if !send(&sender, LifecycleEvent::Authenticated(identity))
        || !send(&sender, LifecycleEvent::ResetNavigation)
        || !send(
            &sender,
            LifecycleEvent::Phase(ConnectionPhase::LoadingNavigation),
        )
    {
        return Err(DegradedReason::Offline);
    }
    let memberships = match client.tenants().await {
        Ok(memberships) => memberships,
        Err(error) => return Err(fail(&sender, &error)),
    };
    let providers = match client.providers().await {
        Ok(providers) => providers,
        Err(error) => return Err(fail(&sender, &error)),
    };
    if !send(&sender, LifecycleEvent::Providers(providers)) {
        return Err(DegradedReason::Offline);
    }
    let mut loaded = LoadedInstance::default();
    for membership in memberships {
        let tenant_id = membership.tenant.id;
        let rooms = match client.rooms(tenant_id).await {
            Ok(rooms) => rooms,
            Err(error) => return Err(fail(&sender, &error)),
        };
        let connections = match client.connection_profiles(tenant_id).await {
            Ok(profiles) => profiles
                .into_iter()
                .map(|profile| ConnectionNavEntry {
                    id: profile.id.0,
                    tenant_id: tenant_id.0,
                    name: profile.name,
                    provider_id: profile.provider_id,
                })
                .collect(),
            Err(error) => return Err(fail(&sender, &error)),
        };
        let mut room_rows = Vec::with_capacity(rooms.len());
        for room in rooms {
            let documents = match client.documents(room.id).await {
                Ok(documents) => documents
                    .into_iter()
                    .map(|document| DocumentNavEntry {
                        id: document.id,
                        room_id: document.room_id,
                        title: document.title,
                        kind: document.kind,
                        position: document.position,
                        snapshot: document.crdt_state,
                    })
                    .collect::<Vec<_>>(),
                Err(error) => return Err(fail(&sender, &error)),
            };
            let workspaces = match client.room_workspaces(room.id).await {
                Ok(workspaces) => workspaces
                    .into_iter()
                    .map(|workspace| WorkspaceNavEntry {
                        id: workspace.id.0,
                        room_id: workspace.room_id,
                        name: workspace.name,
                        git_enabled: workspace.capabilities.git,
                        scheduling_enabled: workspace.capabilities.scheduling,
                    })
                    .collect::<Vec<_>>(),
                Err(error) => return Err(fail(&sender, &error)),
            };
            loaded.workspaces.extend(workspaces.iter().cloned());
            room_rows.push(RoomNavEntry {
                id: room.id,
                tenant_id,
                name: room.name,
                workspaces,
                documents,
            });
        }
        if !send(
            &sender,
            LifecycleEvent::TenantLoaded(TenantNavEntry {
                id: tenant_id,
                name: membership.tenant.name,
                rooms: room_rows,
                connections,
            }),
        ) {
            return Err(DegradedReason::Offline);
        }
    }
    let _ = sender.send(LifecycleEvent::Phase(ConnectionPhase::Ready));
    Ok(loaded)
}

/// Maintain the non-durable room projection. Reconnection remains owned by
/// the instance lifecycle so navigation and presence rebuild together.
pub async fn stream_room_presence(
    client: Client,
    room_id: RoomId,
    client_id: String,
    sender: mpsc::UnboundedSender<PresenceEvent>,
) -> Result<(), DegradedReason> {
    let mut socket = client
        .connect_room_websocket(room_id)
        .await
        .map_err(|error| degraded(&error))?;
    let (attachment_id, presence) = socket
        .attach_with_presence(client_id)
        .await
        .map_err(|error| degraded(&error))?;
    if sender
        .send(PresenceEvent::Joined {
            room_id,
            attachment_id,
            presence,
        })
        .is_err()
    {
        return Ok(());
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            message = socket.next() => {
                let message = message.map_err(|error| degraded(&error))?;
                if sender.send(PresenceEvent::Message(message)).is_err() {
                    return Ok(());
                }
            }
            _ = heartbeat.tick() => {
                socket.heartbeat().await.map_err(|error| degraded(&error))?;
            }
        }
    }
}

pub async fn create_virtual_workspace(
    client: &Client,
    room_id: RoomId,
    name: impl Into<String>,
) -> sift_client_sdk::Result<WorkspaceNavEntry> {
    let workspace = client
        .create_workspace(room_id, CreateWorkspaceRequest { name: name.into() })
        .await?;
    Ok(WorkspaceNavEntry {
        id: workspace.id.0,
        room_id: workspace.room_id,
        name: workspace.name,
        git_enabled: workspace.capabilities.git,
        scheduling_enabled: workspace.capabilities.scheduling,
    })
}

/// Ephemeral room presence/follow projection. It is deliberately absent from
/// presentation serialization and can always be rebuilt after reconnect.
#[derive(Debug, Clone, Default)]
pub struct RoomPresenceProjection {
    pub room_id: Option<RoomId>,
    pub attachment_id: Option<i64>,
    pub participants: Vec<RoomPresence>,
    pub followed_attachment: Option<i64>,
}

impl RoomPresenceProjection {
    pub fn join(&mut self, room_id: RoomId) {
        self.room_id = Some(room_id);
        self.attachment_id = None;
        self.participants.clear();
        self.followed_attachment = None;
    }

    pub fn apply(&mut self, event: PresenceEvent) {
        match event {
            PresenceEvent::Joined {
                room_id,
                attachment_id,
                presence,
            } => {
                self.room_id = Some(room_id);
                self.attachment_id = Some(attachment_id);
                self.participants = presence;
            }
            PresenceEvent::Message(message) => self.ingest(&message),
            PresenceEvent::Left => *self = Self::default(),
        }
    }

    pub fn follow(&mut self, attachment_id: i64) -> bool {
        if self
            .participants
            .iter()
            .any(|presence| presence.attachment_id == attachment_id)
        {
            self.followed_attachment = Some(attachment_id);
            true
        } else {
            false
        }
    }

    pub fn ingest(&mut self, message: &RoomServerMessage) {
        match message {
            RoomServerMessage::Attached { presence, .. }
            | RoomServerMessage::Presence { presence } => {
                self.participants.clone_from(presence);
                if self.followed_attachment.is_some_and(|attachment| {
                    !self
                        .participants
                        .iter()
                        .any(|presence| presence.attachment_id == attachment)
                }) {
                    self.followed_attachment = None;
                }
            }
            RoomServerMessage::ResyncRequired { .. } => {
                self.participants.clear();
                self.followed_attachment = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_replaces_stale_navigation_and_preserves_progress() {
        let mut projection = LifecycleProjection {
            tenants: vec![TenantNavEntry {
                id: TenantId(99),
                name: "stale".into(),
                rooms: vec![],
                connections: vec![],
            }],
            ..Default::default()
        };
        projection.apply(LifecycleEvent::ResetNavigation);
        projection.apply(LifecycleEvent::Phase(ConnectionPhase::LoadingNavigation));
        projection.apply(LifecycleEvent::TenantLoaded(TenantNavEntry {
            id: TenantId(1),
            name: "Personal".into(),
            rooms: vec![],
            connections: vec![ConnectionNavEntry {
                id: 5,
                tenant_id: 1,
                name: "Local PG".into(),
                provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            }],
        }));
        assert_eq!(projection.tenants.len(), 1);
        assert_eq!(projection.tenants[0].id, TenantId(1));
        assert_eq!(projection.status_label(), "Loading workspace…");
    }

    #[test]
    fn instance_selection_restores_known_ids_and_rejects_stale_ones() {
        let instances = vec![
            InstanceSpec {
                id: "local".into(),
                name: "Local".into(),
                base_url: "http://localhost".into(),
                kind: InstanceKind::Local,
            },
            InstanceSpec {
                id: "team".into(),
                name: "Team".into(),
                base_url: "https://sift.example".into(),
                kind: InstanceKind::Hosted,
            },
            InstanceSpec {
                id: "ssh-dev".into(),
                name: "Dev over SSH".into(),
                base_url: "http://127.0.0.1:17474".into(),
                kind: InstanceKind::Ssh,
            },
        ];
        let mut catalog = InstanceCatalog::new(instances, Some("team"));
        assert_eq!(catalog.selected().unwrap().id, "team");
        assert!(catalog.select("ssh-dev"));
        assert_eq!(catalog.selected().unwrap().kind, InstanceKind::Ssh);
        assert!(!catalog.select("removed"));
        assert_eq!(catalog.selected().unwrap().id, "ssh-dev");
    }

    #[test]
    fn presence_and_follow_are_ephemeral_and_drop_departed_target() {
        let mut projection = RoomPresenceProjection::default();
        projection.join(RoomId(7));
        let participant = RoomPresence {
            attachment_id: 42,
            principal_id: 3,
            client_id: "desktop".into(),
            active_document_id: Some(8),
            selection: None,
        };
        projection.ingest(&RoomServerMessage::Presence {
            presence: vec![participant],
        });
        assert!(projection.follow(42));
        projection.ingest(&RoomServerMessage::Presence { presence: vec![] });
        assert_eq!(projection.followed_attachment, None);
    }

    #[test]
    fn degraded_states_have_actionable_labels() {
        for (reason, expected) in [
            (DegradedReason::Offline, "Offline · retry available"),
            (DegradedReason::AuthenticationExpired, "Sign in again"),
            (DegradedReason::AccessRevoked, "Workspace access revoked"),
            (
                DegradedReason::IncompatibleProtocol,
                "Client/server update required",
            ),
        ] {
            let projection = LifecycleProjection {
                phase: ConnectionPhase::Degraded(reason),
                ..Default::default()
            };
            assert_eq!(projection.status_label(), expected);
        }
    }

    #[test]
    fn authentication_and_membership_failures_are_not_treated_as_offline() {
        assert_eq!(
            degraded_http_status(401),
            Some(DegradedReason::AuthenticationExpired)
        );
        assert_eq!(
            degraded_http_status(403),
            Some(DegradedReason::AccessRevoked)
        );
        assert_eq!(degraded_http_status(503), None);
    }
}
