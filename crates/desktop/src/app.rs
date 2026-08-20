use std::collections::HashMap;
use std::sync::Arc;

use gpui::{prelude::*, App, Context, Entity, IntoElement, Window};
use sift_api_types::{
    ConnectionProfileId, CredentialMode, RoomId, TenantId, UpsertConnectionProfileRequest,
};
use sift_client_sdk::{
    Client, Error as ClientError, Ingest, OpenConnectionFromProfileRequest, RoomReplica,
    SessionTokenProvider,
};
use sift_protocol::{ConnectionId, SessionId};
use sift_workspace_ui::{
    ConnectionStatus, EditorMode, ExecutorCommand, ExecutorEvent, PresentationState,
    PresentationStore, Rect, ResultState, RoomDocumentCommand, RoomDocumentEvent, SettingsStore,
    UserSettings, WorkspaceShell,
};

use crate::config::DesktopConfig;
use crate::instances::{
    run_instance_manager, DesktopCredentialStore, InstanceManagerChannels, InstanceStore,
};
use crate::local_server::LocalServerManager;
use crate::platform::{
    current_platform, instance_state_path, presentation_state_path, settings_path, PlatformKind,
};

#[derive(Clone)]
pub enum DesktopServer {
    Local(LocalServerManager),
    Configured {
        local: LocalServerManager,
        instance: sift_workspace_ui::InstanceSpec,
    },
    Remote {
        client: Client,
        instance: sift_workspace_ui::InstanceSpec,
    },
}

impl DesktopServer {
    fn local(runtime_state_dir: std::path::PathBuf) -> Self {
        Self::Local(
            LocalServerManager::bundled(runtime_state_dir).expect("resolving bundled local server"),
        )
    }

    fn from_config(config: DesktopConfig, runtime_state_dir: std::path::PathBuf) -> Self {
        match (config.instance_root, config.remote) {
            (Some(root), None) => Self::configured(root)
                .expect("desktop configuration validated the Sift instance root"),
            (None, Some(remote)) => {
                let mut client = Client::new(&remote.base_url);
                if let Some(token) = remote.bearer_token() {
                    client = client.with_bearer_token(token);
                }
                Self::Remote {
                    client,
                    instance: sift_workspace_ui::InstanceSpec {
                        id: format!("hosted:{}", remote.base_url),
                        name: remote.name,
                        base_url: remote.base_url,
                        kind: sift_workspace_ui::InstanceKind::Hosted,
                    },
                }
            }
            (None, None) => Self::local(runtime_state_dir),
            (Some(_), Some(_)) => unreachable!("desktop configuration targets are exclusive"),
        }
    }

    pub(crate) fn remote(
        profile: sift_workspace_ui::SavedServerProfile,
        token: Option<String>,
    ) -> Self {
        let mut client = Client::new(&profile.base_url);
        if let Some(token) = token {
            client = client.with_bearer_token(token);
        }
        Self::Remote {
            client,
            instance: sift_workspace_ui::InstanceSpec {
                id: format!("hosted:{}", profile.id),
                name: profile.name,
                base_url: profile.base_url,
                kind: sift_workspace_ui::InstanceKind::Hosted,
            },
        }
    }

    pub(crate) fn configured(root: std::path::PathBuf) -> Result<Self, String> {
        let configured = sift_server::instance_runtime::InstanceRoot::open(&root)
            .map_err(|error| format!("validating instance root failed: {error:#}"))?;
        let instance = sift_workspace_ui::InstanceSpec {
            id: format!("config:{}", configured.manifest.manifest_id),
            name: configured.manifest.name.clone(),
            base_url: configured.manifest.server.bind.clone(),
            kind: sift_workspace_ui::InstanceKind::Local,
        };
        Ok(Self::Configured {
            local: LocalServerManager::configured(configured.root)?,
            instance,
        })
    }

    pub(crate) async fn client(&self) -> Result<Client, String> {
        match self {
            Self::Local(local) => local.ensure_ready().await,
            Self::Configured { local, .. } => local.ensure_ready().await,
            Self::Remote { client, .. } => Ok(client.clone()),
        }
    }

    pub(crate) fn with_session_tokens(
        &self,
        session_tokens: SessionTokenProvider,
    ) -> Result<Self, String> {
        match self {
            Self::Local(_) | Self::Configured { .. } => {
                Err("Local Sift uses its built-in identity".into())
            }
            Self::Remote { instance, .. } => Ok(Self::Remote {
                client: Client::new(&instance.base_url).with_session_tokens(session_tokens),
                instance: instance.clone(),
            }),
        }
    }

    pub(crate) fn without_authentication(&self) -> Result<Self, String> {
        match self {
            Self::Local(_) | Self::Configured { .. } => {
                Err("Local Sift uses its built-in identity".into())
            }
            Self::Remote { instance, .. } => Ok(Self::Remote {
                client: Client::new(&instance.base_url),
                instance: instance.clone(),
            }),
        }
    }

    pub(crate) fn instance(&self) -> sift_workspace_ui::InstanceSpec {
        match self {
            Self::Local(_) => sift_workspace_ui::InstanceSpec {
                id: "local".into(),
                name: "Local Sift".into(),
                base_url: "http://127.0.0.1:7474".into(),
                kind: sift_workspace_ui::InstanceKind::Local,
            },
            Self::Configured { instance, .. } => instance.clone(),
            Self::Remote { instance, .. } => instance.clone(),
        }
    }

    pub(crate) fn acquire_local_lease(&self) -> Option<crate::local_server::LocalServerLease> {
        match self {
            Self::Local(local) => Some(local.acquire()),
            Self::Configured { local, .. } => Some(local.acquire()),
            Self::Remote { .. } => None,
        }
    }

    pub(crate) fn configured_root(&self) -> Option<&std::path::Path> {
        match self {
            Self::Configured { local, .. } => local.instance_root(),
            Self::Local(_) | Self::Remote { .. } => None,
        }
    }
}

/// Process-wide desktop services. Product state remains behind the SDK; this
/// object owns only platform and presentation concerns. Presentation state is
/// local to this OS account/desktop installation and is never synchronized to
/// a Sift server or keyed by a remote principal.
pub struct SiftApp {
    pub platform: PlatformKind,
    pub presentation_store: Arc<PresentationStore>,
    pub settings_store: Arc<SettingsStore>,
    pub settings: UserSettings,
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub server: DesktopServer,
    pub instance_store: InstanceStore,
    pub credentials: DesktopCredentialStore,
    pub saved_servers: Vec<sift_workspace_ui::SavedServerProfile>,
    pub instance_roots: Vec<sift_workspace_ui::SavedInstanceRoot>,
    pub local_target: DesktopServer,
    startup_remote: bool,
}

#[derive(Clone)]
pub struct WindowServices {
    store: Arc<PresentationStore>,
    settings_store: Arc<SettingsStore>,
    settings: UserSettings,
    runtime: Arc<tokio::runtime::Runtime>,
    server: DesktopServer,
    instance_store: InstanceStore,
    credentials: DesktopCredentialStore,
    saved_servers: Vec<sift_workspace_ui::SavedServerProfile>,
    instance_roots: Vec<sift_workspace_ui::SavedInstanceRoot>,
    local_target: DesktopServer,
    restored_profile_id: Option<String>,
}

impl SiftApp {
    pub fn new(config: DesktopConfig) -> Self {
        let state_path = presentation_state_path();
        let presentation_store = Arc::new(PresentationStore::new(&state_path));
        let settings_store = Arc::new(SettingsStore::new(settings_path()));
        let settings = load_user_settings(&settings_store, &presentation_store.load());
        let runtime_state_dir = state_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("runtime");
        let instance_store = InstanceStore::new(instance_state_path());
        let saved_servers = instance_store.load();
        let mut instance_roots = instance_store.load_roots();
        let local_target = DesktopServer::local(runtime_state_dir.clone());
        let configured_root = config.instance_root.clone();
        let startup_remote = config.remote.is_some() || configured_root.is_some();
        let server = if startup_remote {
            DesktopServer::from_config(config, runtime_state_dir)
        } else {
            local_target.clone()
        };
        if let Some(root) = configured_root {
            if let DesktopServer::Configured { instance, .. } = &server {
                let saved = sift_workspace_ui::SavedInstanceRoot {
                    manifest_id: instance.id.trim_start_matches("config:").to_owned(),
                    name: instance.name.clone(),
                    root,
                };
                if let Some(index) = instance_roots
                    .iter()
                    .position(|candidate| candidate.manifest_id == saved.manifest_id)
                {
                    instance_roots[index] = saved;
                } else {
                    instance_roots.push(saved);
                }
                instance_roots.sort_by(|left, right| left.name.cmp(&right.name));
                let _ = instance_store.save_roots(&saved_servers, &instance_roots);
            }
        }
        Self {
            platform: current_platform(),
            presentation_store,
            settings_store,
            settings,
            runtime: Arc::new(tokio::runtime::Runtime::new().expect("creating client runtime")),
            server,
            instance_store,
            credentials: DesktopCredentialStore,
            saved_servers,
            instance_roots,
            local_target,
            startup_remote,
        }
    }

    pub fn restore(&self, displays: &[Rect]) -> PresentationState {
        self.presentation_store
            .load()
            .recover_for_displays(displays)
    }

    pub fn window_services(&self, state: &PresentationState) -> WindowServices {
        let restored_profile = restored_server_profile(
            state.workspace.instance_id.as_deref(),
            &self.saved_servers,
            self.startup_remote,
        );
        WindowServices {
            store: self.presentation_store.clone(),
            settings_store: self.settings_store.clone(),
            settings: self
                .settings_store
                .load()
                .unwrap_or_else(|_| self.settings.clone()),
            runtime: self.runtime.clone(),
            server: restored_profile
                .as_ref()
                .map(|profile| DesktopServer::remote(profile.clone(), None))
                .unwrap_or_else(|| self.server.clone()),
            instance_store: self.instance_store.clone(),
            credentials: self.credentials.clone(),
            saved_servers: self.saved_servers.clone(),
            instance_roots: self.instance_roots.clone(),
            local_target: self.local_target.clone(),
            restored_profile_id: restored_profile.map(|profile| profile.id),
        }
    }
}

fn load_user_settings(store: &SettingsStore, presentation: &PresentationState) -> UserSettings {
    let mut fallback = UserSettings::default();
    if presentation.legacy_vim_mode_default {
        fallback.editor.default_mode = EditorMode::Vim;
    }
    match store.load() {
        Ok(settings) => settings,
        Err(_) if !store.path().exists() => {
            let _ = store.save(&fallback);
            fallback
        }
        Err(_) => fallback,
    }
}

fn restored_server_profile(
    instance_id: Option<&str>,
    profiles: &[sift_workspace_ui::SavedServerProfile],
    startup_remote: bool,
) -> Option<sift_workspace_ui::SavedServerProfile> {
    if startup_remote {
        return None;
    }
    let profile_id = instance_id?.strip_prefix("hosted:")?;
    profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned()
}

/// Window-level ownership boundary. Additional windows can each own exactly
/// one virtual workspace without adding product state to `SiftApp`.
pub struct SiftWindow {
    workspace: Entity<WorkspaceShell>,
    // The spawned lifecycle, query, and instance-manager tasks are owned by
    // this runtime. Keeping it with the window prevents dropping the last
    // runtime handle immediately after application startup.
    _runtime: Arc<tokio::runtime::Runtime>,
}

impl SiftWindow {
    pub fn new(
        state: PresentationState,
        services: WindowServices,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let WindowServices {
            store,
            settings_store,
            settings,
            runtime,
            server,
            instance_store,
            credentials,
            saved_servers,
            instance_roots,
            local_target,
            restored_profile_id,
        } = services;
        let state = prepare_state_for_instance(state, &server.instance());
        let restored_workspace_id = state.workspace.workspace_id;
        let workspace = cx.new(|cx| {
            WorkspaceShell::new(
                state,
                settings,
                Some(store),
                Some(settings_store),
                window,
                cx,
            )
        });
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (presence_sender, presence_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (document_sender, document_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (document_event_sender, document_event_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (instance_sender, instance_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (instance_event_sender, instance_event_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (target_sender, target_receiver) = tokio::sync::watch::channel(server.clone());
        workspace.update(cx, |workspace, cx| {
            workspace.attach_lifecycle(receiver, window, cx);
            workspace.attach_presence(presence_receiver, cx);
            workspace.attach_room_documents(document_sender, document_event_receiver, cx);
            workspace.attach_executor(command_sender, event_receiver, cx);
            workspace.attach_instance_manager(
                instance_sender,
                instance_event_receiver,
                saved_servers.clone(),
                cx,
            );
        });
        std::mem::drop(runtime.spawn(supervise_instances(
            target_receiver.clone(),
            restored_workspace_id,
            sender,
            presence_sender,
        )));
        std::mem::drop(runtime.spawn(run_query_executor(
            target_receiver.clone(),
            command_receiver,
            event_sender,
        )));
        std::mem::drop(runtime.spawn(run_room_document_supervisor(
            target_receiver.clone(),
            document_receiver,
            document_event_sender,
        )));
        std::mem::drop(runtime.spawn(run_instance_manager(
            instance_store,
            credentials,
            saved_servers,
            instance_roots,
            InstanceManagerChannels {
                commands: instance_receiver,
                events: instance_event_sender,
                targets: target_sender,
            },
            local_target,
            restored_profile_id,
        )));
        Self {
            workspace,
            _runtime: runtime,
        }
    }
}

fn prepare_state_for_instance(
    mut state: PresentationState,
    instance: &sift_workspace_ui::InstanceSpec,
) -> PresentationState {
    if state.workspace.instance_id.as_deref() != Some(instance.id.as_str()) {
        if let Some(previous) = state.workspace.instance_id.clone() {
            state
                .instance_workspaces
                .insert(previous, state.workspace.clone());
        }
        state.workspace = match state.instance_workspaces.remove(&instance.id) {
            Some(workspace) => workspace,
            None => {
                let mut workspace = PresentationState::default().workspace;
                workspace.workspace_id = None;
                workspace
            }
        };
        state.workspace.instance_id = Some(instance.id.clone());
    } else {
        state.instance_workspaces.remove(&instance.id);
    }
    state
}

/// An opened, reusable execution target: one client, session, and connection.
struct QueryContext {
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    profile_id: i64,
}

/// Owns the SDK client and the current session/connection. Connection is
/// explicit — the user picks a profile in the UI; the executor opens it and
/// runs queries against it. The UI thread never touches the SDK directly.
async fn run_query_executor(
    mut targets: tokio::sync::watch::Receiver<DesktopServer>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<ExecutorCommand>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let mut context: Option<QueryContext> = None;
    let mut active_queries: HashMap<u64, (u64, tokio::sync::mpsc::UnboundedSender<QueryControl>)> =
        HashMap::new();
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            changed = targets.changed() => {
                if changed.is_err() {
                    return;
                }
                cancel_active_queries(&mut active_queries);
                if let Some(previous) = context.take() {
                    let _ = previous.client.close_session(previous.session).await;
                }
                if events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected)).is_err() {
                    return;
                }
                continue;
            }
        };
        let Some(command) = command else {
            return;
        };
        match command {
            ExecutorCommand::Connect {
                tenant_id,
                profile_id,
                name,
            } => {
                cancel_active_queries(&mut active_queries);
                if let Some(previous) = context.take() {
                    let _ = previous.client.close_session(previous.session).await;
                }
                let server = targets.borrow().clone();
                match open_query_context(&server, tenant_id, profile_id).await {
                    Ok(opened) => {
                        if events
                            .send(ExecutorEvent::Connection(ConnectionStatus::Connected {
                                profile_id,
                                name,
                            }))
                            .is_err()
                        {
                            return;
                        }
                        let schema_event = load_schema(&opened).await;
                        context = Some(opened);
                        if events.send(schema_event).is_err() {
                            return;
                        }
                    }
                    Err(reason) => {
                        if events
                            .send(ExecutorEvent::Connection(ConnectionStatus::Failed {
                                profile_id,
                                reason,
                            }))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            ExecutorCommand::Disconnect => {
                cancel_active_queries(&mut active_queries);
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
            ExecutorCommand::RefreshSchema => {
                let Some(opened) = context.as_ref() else {
                    continue;
                };
                if events.send(load_schema(opened).await).is_err() {
                    return;
                }
            }
            ExecutorCommand::Execute {
                item_id,
                execution_id,
                sql,
            } => {
                let Some(opened) = context.as_ref() else {
                    if events
                        .send(ExecutorEvent::Execution {
                            item_id,
                            execution_id,
                            state: ResultState::Unavailable(
                                "Not connected — pick a connection to run this.".into(),
                            ),
                        })
                        .is_err()
                    {
                        return;
                    }
                    continue;
                };
                if let Some((_, previous)) = active_queries.remove(&item_id) {
                    let _ = previous.send(QueryControl::Cancel);
                }
                let (control_sender, control_receiver) = tokio::sync::mpsc::unbounded_channel();
                active_queries.insert(item_id, (execution_id, control_sender));
                let client = opened.client.clone();
                let session = opened.session;
                let connection = opened.connection;
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    run_streamed_query(
                        QueryRun {
                            client,
                            session,
                            connection,
                            item_id,
                            execution_id,
                            sql,
                        },
                        control_receiver,
                        events,
                    )
                    .await;
                }));
            }
            ExecutorCommand::Cancel {
                item_id,
                execution_id,
            } => {
                if active_queries
                    .get(&item_id)
                    .is_some_and(|(active, _)| *active == execution_id)
                {
                    if let Some((_, control)) = active_queries.remove(&item_id) {
                        let _ = control.send(QueryControl::Cancel);
                    }
                }
            }
            ExecutorCommand::CreateConnectionProfile {
                tenant_id,
                name,
                provider_id,
                configuration,
                credentials,
            } => {
                if let Some(previous) = context.take() {
                    let _ = previous.client.close_session(previous.session).await;
                }
                let server = targets.borrow().clone();
                let result = create_connection_profile(
                    &server,
                    tenant_id,
                    name,
                    provider_id,
                    configuration,
                    credentials,
                )
                .await;
                match result {
                    Ok(entry) => {
                        let connection_error =
                            match open_query_context(&server, tenant_id, entry.id).await {
                                Ok(opened) => {
                                    let _ = events.send(ExecutorEvent::Connection(
                                        ConnectionStatus::Connected {
                                            profile_id: entry.id,
                                            name: entry.name.clone(),
                                        },
                                    ));
                                    let schema_event = load_schema(&opened).await;
                                    context = Some(opened);
                                    let _ = events.send(schema_event);
                                    None
                                }
                                Err(reason) => {
                                    let _ = events.send(ExecutorEvent::Connection(
                                        ConnectionStatus::Failed {
                                            profile_id: entry.id,
                                            reason: reason.clone(),
                                        },
                                    ));
                                    Some(reason)
                                }
                            };
                        if events
                            .send(ExecutorEvent::ProfileCreated {
                                entry,
                                connection_error,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(message) => {
                        if events
                            .send(ExecutorEvent::ProfileCreationFailed(message))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            ExecutorCommand::DeleteConnectionProfile {
                tenant_id,
                profile_id,
            } => {
                if context
                    .as_ref()
                    .is_some_and(|opened| opened.profile_id == profile_id)
                {
                    if let Some(opened) = context.take() {
                        let _ = opened.client.close_session(opened.session).await;
                    }
                }
                let server = targets.borrow().clone();
                let result = delete_connection_profile(&server, tenant_id, profile_id).await;
                let event = match result {
                    Ok(()) => ExecutorEvent::ProfileDeleted {
                        tenant_id,
                        profile_id,
                    },
                    Err(error) => ExecutorEvent::ProfileDeletionFailed(error),
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            ExecutorCommand::LoadTableDefinition { item_id, source } => {
                let event = match context.as_ref() {
                    Some(opened) if opened.profile_id == source.profile_id => {
                        load_table_definition(opened, item_id, &source).await
                    }
                    _ => ExecutorEvent::TableDefinitionFailed {
                        item_id,
                        message: "Connect to this table before loading its definition".into(),
                    },
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            ExecutorCommand::PreviewTableMutation {
                item_id,
                expected_catalog_revision,
                mutation,
            } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .preview_catalog_diagram_mutation(
                            opened.session,
                            opened.connection,
                            sift_protocol::PreviewCatalogDiagramMutationRequest {
                                expected_catalog_revision,
                                mutation,
                                options: sift_protocol::MigrationOptions::default(),
                            },
                        )
                        .await
                        .map(|plan| ExecutorEvent::TableMigrationPreviewed {
                            item_id,
                            plan: Box::new(plan),
                        })
                        .unwrap_or_else(|error| ExecutorEvent::TableMigrationFailed {
                            item_id,
                            message: format!("previewing table migration failed: {error}"),
                        }),
                    None => ExecutorEvent::TableMigrationFailed {
                        item_id,
                        message: "Connect to this table before previewing changes".into(),
                    },
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            ExecutorCommand::ApplyTableMigration { item_id, request } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .apply_migration(opened.session, opened.connection, request)
                        .await
                        .map(|run| ExecutorEvent::TableMigrationApplied {
                            item_id,
                            run: Box::new(run),
                        })
                        .unwrap_or_else(|error| ExecutorEvent::TableMigrationFailed {
                            item_id,
                            message: format!("applying table migration failed: {error}"),
                        }),
                    None => ExecutorEvent::TableMigrationFailed {
                        item_id,
                        message: "Connect to this table before applying changes".into(),
                    },
                };
                if events.send(event).is_err() {
                    return;
                }
                if let Some(opened) = context.as_ref() {
                    if events.send(load_schema(opened).await).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

enum QueryControl {
    Cancel,
}

fn cancel_active_queries(
    active_queries: &mut HashMap<u64, (u64, tokio::sync::mpsc::UnboundedSender<QueryControl>)>,
) {
    for (_, (_, control)) in active_queries.drain() {
        let _ = control.send(QueryControl::Cancel);
    }
}

async fn delete_connection_profile(
    server: &DesktopServer,
    tenant_id: i64,
    profile_id: i64,
) -> Result<(), String> {
    server
        .client()
        .await?
        .delete_connection_profile(TenantId(tenant_id), ConnectionProfileId(profile_id))
        .await
        .map_err(|error| format!("deleting connection profile failed: {error}"))
}

async fn load_schema(context: &QueryContext) -> ExecutorEvent {
    match context
        .client
        .schema(context.session, context.connection)
        .await
    {
        Ok(snapshot) => ExecutorEvent::SchemaLoaded {
            profile_id: context.profile_id,
            snapshot: Box::new(snapshot),
        },
        Err(error) => ExecutorEvent::SchemaLoadFailed {
            profile_id: context.profile_id,
            message: format!("loading database schema failed: {error}"),
        },
    }
}

async fn load_table_definition(
    context: &QueryContext,
    item_id: u64,
    source: &sift_workspace_ui::DatabaseObjectSource,
) -> ExecutorEvent {
    let request = sift_protocol::CatalogGraphRequest {
        options: sift_protocol::CatalogGraphOptions {
            schemas: Some(vec![source.schema.clone()]),
            include_definitions: true,
            max_nodes: Some(10_000),
            ..Default::default()
        },
        refresh: false,
    };
    context
        .client
        .catalog_graph(context.session, context.connection, request)
        .await
        .map(|graph| ExecutorEvent::TableDefinitionLoaded {
            item_id,
            graph: Box::new(graph),
        })
        .unwrap_or_else(|error| ExecutorEvent::TableDefinitionFailed {
            item_id,
            message: format!("loading table definition failed: {error}"),
        })
}

async fn create_connection_profile(
    server: &DesktopServer,
    tenant_id: i64,
    name: String,
    provider_id: sift_protocol::ProviderId,
    configuration: serde_json::Value,
    credentials: Option<serde_json::Value>,
) -> Result<sift_workspace_ui::ConnectionNavEntry, String> {
    let client = server.client().await?;
    let profile = client
        .upsert_connection_profile(UpsertConnectionProfileRequest {
            tenant_id,
            name: name.clone(),
            provider_id,
            configuration,
            credentials,
            credential_mode: CredentialMode::Shared,
            tags: Vec::new(),
        })
        .await
        .map_err(|error| format!("saving connection profile failed: {error}"))?;
    Ok(sift_workspace_ui::ConnectionNavEntry {
        id: profile.id.0,
        tenant_id,
        name,
        provider_id: profile.provider_id,
    })
}

struct QueryRun {
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    item_id: u64,
    execution_id: u64,
    sql: String,
}

async fn run_streamed_query(
    run: QueryRun,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let QueryRun {
        client,
        session,
        connection,
        item_id,
        execution_id,
        sql,
    } = run;
    let started = tokio::select! {
        stream = client.start_query_stream(session, connection, sql) => stream,
        control = controls.recv() => {
            if matches!(control, Some(QueryControl::Cancel)) {
                let _ = events.send(ExecutorEvent::Execution {
                    item_id,
                    execution_id,
                    state: ResultState::Cancelled,
                });
            }
            return;
        }
    };
    let mut stream = match started {
        Ok(stream) => stream,
        Err(error) => {
            send_execution_error(item_id, execution_id, error, &events);
            return;
        }
    };
    let cursor_id = stream.cursor_id();
    if events
        .send(ExecutorEvent::ExecutionStarted {
            item_id,
            execution_id,
            cursor_id,
        })
        .is_err()
    {
        let _ = stream.cancel().await;
        return;
    }

    loop {
        let next = tokio::select! {
            page = stream.next_page() => page,
            control = controls.recv() => {
                if matches!(control, Some(QueryControl::Cancel)) {
                    let _ = stream.cancel().await;
                    let _ = events.send(ExecutorEvent::Execution {
                        item_id,
                        execution_id,
                        state: ResultState::Cancelled,
                    });
                }
                return;
            }
        };
        let (seq, page) = match next {
            Ok(page) => page,
            Err(error) => {
                send_execution_error(item_id, execution_id, error, &events);
                return;
            }
        };
        if matches!(
            &page,
            sift_protocol::Page::Error { error }
                if error.code == sift_protocol::Code::CursorEvicted
                    && error.resume_url.is_some()
        ) {
            resume_spilled_query(
                &client,
                cursor_id,
                item_id,
                execution_id,
                &mut controls,
                &events,
            )
            .await;
            return;
        }
        let terminal = matches!(
            page,
            sift_protocol::Page::Done { .. } | sift_protocol::Page::Error { .. }
        );
        let (acknowledge, consumed) = tokio::sync::oneshot::channel();
        if events
            .send(ExecutorEvent::ExecutionPage {
                item_id,
                execution_id,
                cursor_id,
                page,
                acknowledge,
            })
            .is_err()
        {
            let _ = stream.cancel().await;
            return;
        }
        if terminal {
            return;
        }
        tokio::select! {
            acknowledged = consumed => {
                if acknowledged.is_err() || stream.acknowledge(seq).await.is_err() {
                    let _ = stream.cancel().await;
                    return;
                }
            }
            control = controls.recv() => {
                if matches!(control, Some(QueryControl::Cancel)) {
                    let _ = stream.cancel().await;
                    let _ = events.send(ExecutorEvent::Execution {
                        item_id,
                        execution_id,
                        state: ResultState::Cancelled,
                    });
                }
                return;
            }
        }
    }
}

async fn resume_spilled_query(
    client: &Client,
    cursor_id: sift_protocol::CursorId,
    item_id: u64,
    execution_id: u64,
    controls: &mut tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    loop {
        let batch = tokio::select! {
            batch = client.read_spilled_page_batch(cursor_id, None, Some(1)) => batch,
            control = controls.recv() => {
                if matches!(control, Some(QueryControl::Cancel)) {
                    let _ = client.delete_spilled_cursor(cursor_id).await;
                    let _ = events.send(ExecutorEvent::Execution {
                        item_id,
                        execution_id,
                        state: ResultState::Cancelled,
                    });
                }
                return;
            }
        };
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                send_execution_error(item_id, execution_id, error, events);
                return;
            }
        };
        if batch.cursor_id != cursor_id || (batch.pages.is_empty() && !batch.done) {
            let _ = events.send(ExecutorEvent::Execution {
                item_id,
                execution_id,
                state: ResultState::Failed("invalid spilled cursor page sequence".into()),
            });
            return;
        }
        let mut saw_terminal = false;
        for page in batch.pages {
            saw_terminal |= matches!(
                &page,
                sift_protocol::Page::Done { .. } | sift_protocol::Page::Error { .. }
            );
            let (acknowledge, consumed) = tokio::sync::oneshot::channel();
            if events
                .send(ExecutorEvent::ExecutionPage {
                    item_id,
                    execution_id,
                    cursor_id,
                    page,
                    acknowledge,
                })
                .is_err()
            {
                let _ = client.delete_spilled_cursor(cursor_id).await;
                return;
            }
            if saw_terminal {
                return;
            }
            tokio::select! {
                acknowledged = consumed => {
                    if acknowledged.is_err() {
                        return;
                    }
                }
                control = controls.recv() => {
                    if matches!(control, Some(QueryControl::Cancel)) {
                        let _ = client.delete_spilled_cursor(cursor_id).await;
                        let _ = events.send(ExecutorEvent::Execution {
                            item_id,
                            execution_id,
                            state: ResultState::Cancelled,
                        });
                    }
                    return;
                }
            }
        }
        if batch.done {
            if !saw_terminal {
                let (acknowledge, _) = tokio::sync::oneshot::channel();
                let _ = events.send(ExecutorEvent::ExecutionPage {
                    item_id,
                    execution_id,
                    cursor_id,
                    page: sift_protocol::Page::Done {
                        affected_rows: None,
                        warnings: vec![sift_protocol::DriverWarning::new(
                            "Cursor resumed after eviction; only retained spill pages are available.",
                        )],
                    },
                    acknowledge,
                });
            }
            return;
        }
    }
}

fn send_execution_error(
    item_id: u64,
    execution_id: u64,
    error: ClientError,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let transport = matches!(
        &error,
        ClientError::Transport(_) | ClientError::WebSocket(_)
    );
    let message = match error {
        ClientError::Server { error, .. } => error.message,
        other => other.to_string(),
    };
    if transport {
        let _ = events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected));
    }
    let _ = events.send(ExecutorEvent::Execution {
        item_id,
        execution_id,
        state: ResultState::from_execution_error(transport, message),
    });
}

/// Open a session and a connection for the chosen tenant/profile. Ids come from
/// the UI's connection picker, so no discovery/guessing happens here.
async fn open_query_context(
    server: &DesktopServer,
    tenant_id: i64,
    profile_id: i64,
) -> Result<QueryContext, String> {
    let client = server.client().await?;
    let session = client
        .open_session_for_tenant(None, Some(tenant_id))
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
        profile_id,
    })
}

async fn supervise_instances(
    mut targets: tokio::sync::watch::Receiver<DesktopServer>,
    mut restored_workspace_id: Option<i64>,
    sender: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::LifecycleEvent>,
    presence_sender: tokio::sync::mpsc::UnboundedSender<sift_workspace_ui::PresenceEvent>,
) {
    let mut attempt = 0_u32;
    loop {
        if sender.is_closed() {
            return;
        }
        let server = targets.borrow().clone();
        let _local_server_lease = server.acquire_local_lease();
        let instance = server.instance();
        let client = match server.client().await {
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
        let load =
            sift_workspace_ui::load_instance(client.clone(), instance.clone(), sender.clone());
        let loaded = match tokio::select! {
            loaded = load => Some(loaded),
            changed = targets.changed() => {
                if changed.is_err() { return; }
                restored_workspace_id = None;
                None
            }
        } {
            None => continue,
            Some(Ok(loaded)) => loaded,
            Some(Err(sift_workspace_ui::DegradedReason::Offline)) => {
                attempt = attempt.saturating_add(1);
                if !wait_to_reconnect(attempt, &sender).await {
                    return;
                }
                continue;
            }
            Some(Err(
                sift_workspace_ui::DegradedReason::AuthenticationExpired
                | sift_workspace_ui::DegradedReason::AccessRevoked,
            )) => {
                if targets.changed().await.is_err() {
                    return;
                }
                restored_workspace_id = None;
                continue;
            }
            Some(Err(_)) => return,
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
                changed = targets.changed() => {
                    if changed.is_err() { return; }
                    restored_workspace_id = None;
                    let _ = presence_sender.send(sift_workspace_ui::PresenceEvent::Left);
                    continue;
                },
            }
        } else {
            tokio::select! {
                result = wait_for_server_loss(&client) => result,
                changed = targets.changed() => {
                    if changed.is_err() { return; }
                    restored_workspace_id = None;
                    continue;
                },
            }
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
                if targets.changed().await.is_err() {
                    return;
                }
                restored_workspace_id = None;
                continue;
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

async fn run_room_document_supervisor(
    mut targets: tokio::sync::watch::Receiver<DesktopServer>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<RoomDocumentCommand>,
    events: tokio::sync::mpsc::UnboundedSender<RoomDocumentEvent>,
) {
    let mut documents: HashMap<
        i64,
        (
            tokio::task::JoinHandle<()>,
            tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        ),
    > = HashMap::new();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    RoomDocumentCommand::Create {
                        instance_id,
                        room_id,
                        title,
                        position,
                    } => {
                        let server = targets.borrow().clone();
                        if server.instance().id != instance_id {
                            continue;
                        }
                        let result = async {
                            let client = server.client().await?;
                            client
                                .create_document(
                                    RoomId(room_id),
                                    sift_api_types::CreateDocumentRequest {
                                        kind: "query".into(),
                                        title,
                                        initial_text: None,
                                        position,
                                        connection_profile_id: None,
                                    },
                                )
                                .await
                                .map_err(|error| format!("creating query document failed: {error}"))
                        }
                        .await;
                        match result {
                            Ok(document) => {
                                let _ = events.send(RoomDocumentEvent::Created(
                                    sift_workspace_ui::DocumentNavEntry {
                                        id: document.id,
                                        room_id: document.room_id,
                                        title: document.title,
                                        kind: document.kind,
                                        position: document.position,
                                        snapshot: document.crdt_state,
                                    },
                                ));
                            }
                            Err(message) => {
                                let _ = events.send(RoomDocumentEvent::ServiceFailed(message));
                            }
                        }
                    }
                    RoomDocumentCommand::Open { source, snapshot } => {
                        if targets.borrow().instance().id != source.instance_id {
                            continue;
                        }
                        if documents
                            .get(&source.document_id)
                            .is_some_and(|(task, _)| !task.is_finished())
                        {
                            continue;
                        }
                        documents.remove(&source.document_id);
                        let server = targets.borrow().clone();
                        let events = events.clone();
                        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                        let document_id = source.document_id;
                        let task = tokio::spawn(async move {
                            let result = run_room_document(
                                server,
                                source,
                                snapshot,
                                receiver,
                                events.clone(),
                            ).await;
                            if let Err(message) = result {
                                let _ = events.send(RoomDocumentEvent::Failed {
                                    document_id,
                                    message,
                                });
                            }
                        });
                        documents.insert(document_id, (task, sender));
                    }
                    RoomDocumentCommand::Update { document_id, update } => {
                        if let Some((_, sender)) = documents.get(&document_id) {
                            let _ = sender.send(update);
                        }
                    }
                    RoomDocumentCommand::Close { document_id } => {
                        if let Some((task, _)) = documents.remove(&document_id) {
                            task.abort();
                        }
                    }
                }
            }
            changed = targets.changed() => {
                if changed.is_err() { break; }
                for (_, (task, _)) in documents.drain() {
                    task.abort();
                }
            }
        }
    }
    for (_, (task, _)) in documents {
        task.abort();
    }
}

async fn run_room_document(
    server: DesktopServer,
    source: sift_workspace_ui::RoomDocumentSource,
    snapshot: Vec<u8>,
    mut updates: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    events: tokio::sync::mpsc::UnboundedSender<RoomDocumentEvent>,
) -> Result<(), String> {
    let client = server.client().await?;
    let mut replica = RoomReplica::new(
        source.document_id,
        sift_doc::random_peer_id(),
        Some(&snapshot),
    )
    .map_err(|error| format!("loading replica failed: {error}"))?;
    let mut room = client.persistent_room(
        RoomId(source.room_id),
        format!(
            "sift-desktop-document-{}-{}",
            std::process::id(),
            source.document_id
        ),
    );
    room.connect(&mut replica)
        .await
        .map_err(|error| format!("room sync failed: {error}"))?;
    let _ = events.send(RoomDocumentEvent::Text {
        document_id: source.document_id,
        snapshot: replica
            .persist()
            .map_err(|error| format!("snapshotting room replica failed: {error}"))?
            .1,
        synced: true,
    });
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            update = updates.recv() => {
                let Some(update) = update else { return Ok(()) };
                let message = replica.import_local_update(&update)
                    .map_err(|error| format!("applying editor update failed: {error}"))?;
                room.submit(&mut replica, message)
                    .await
                    .map_err(|error| format!("saving editor update failed: {error}"))?;
                // Do not momentarily roll the editor back to an intermediate
                // ACK when more local updates are already queued.
                if updates.is_empty() {
                    let _ = events.send(RoomDocumentEvent::Text {
                        document_id: source.document_id,
                        snapshot: replica
                            .persist()
                            .map_err(|error| format!("snapshotting room replica failed: {error}"))?
                            .1,
                        synced: true,
                    });
                }
            }
            incoming = room.next(&mut replica) => {
                match incoming.map_err(|error| format!("receiving room update failed: {error}"))? {
                    Ingest::Progress | Ingest::Acked(_) | Ingest::Synced(_) => {
                        let _ = events.send(RoomDocumentEvent::Text {
                            document_id: source.document_id,
                            snapshot: replica
                                .persist()
                                .map_err(|error| format!("snapshotting room replica failed: {error}"))?
                                .1,
                            synced: replica.pending_count() == 0,
                        });
                    }
                    Ingest::Error { message, .. } => return Err(message),
                    Ingest::Resync | Ingest::Ignored => {}
                }
            }
            _ = heartbeat.tick() => {
                room.heartbeat(&mut replica)
                    .await
                    .map_err(|error| format!("room heartbeat failed: {error}"))?;
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

    #[test]
    fn changing_instances_drops_a_stale_workspace_selection() {
        let mut state = PresentationState::default();
        state.workspace.instance_id = Some("local".into());
        state.workspace.workspace_id = Some(42);
        let remote = sift_workspace_ui::InstanceSpec {
            id: "hosted:https://sift.lan".into(),
            name: "LAN".into(),
            base_url: "https://sift.lan".into(),
            kind: sift_workspace_ui::InstanceKind::Hosted,
        };

        let state = prepare_state_for_instance(state, &remote);

        assert_eq!(
            state.workspace.instance_id.as_deref(),
            Some(remote.id.as_str())
        );
        assert_eq!(state.workspace.workspace_id, None);
    }

    #[test]
    fn changing_instances_restores_that_instances_workspace_presentation() {
        let mut state = PresentationState::default();
        state.workspace.workspace_id = Some(42);
        state.workspace.panes[0].items[0].title = "Local query".into();
        let remote = sift_workspace_ui::InstanceSpec {
            id: "hosted:team".into(),
            name: "Team".into(),
            base_url: "https://sift.team".into(),
            kind: sift_workspace_ui::InstanceKind::Hosted,
        };
        let mut remote_workspace = PresentationState::default().workspace;
        remote_workspace.instance_id = Some(remote.id.clone());
        remote_workspace.workspace_id = Some(77);
        remote_workspace.panes[0].items[0].title = "Team query".into();
        state
            .instance_workspaces
            .insert(remote.id.clone(), remote_workspace);

        let state = prepare_state_for_instance(state, &remote);

        assert_eq!(state.workspace.workspace_id, Some(77));
        assert_eq!(state.workspace.panes[0].items[0].title, "Team query");
        assert_eq!(
            state.instance_workspaces["local"].panes[0].items[0].title,
            "Local query"
        );
    }

    #[test]
    fn saved_instance_restores_unless_startup_configuration_overrides_it() {
        let profile = sift_workspace_ui::SavedServerProfile {
            id: "server-one".into(),
            name: "LAN".into(),
            base_url: "https://sift.lan".into(),
            has_saved_token: true,
        };
        assert_eq!(
            restored_server_profile(
                Some("hosted:server-one"),
                std::slice::from_ref(&profile),
                false
            ),
            Some(profile.clone())
        );
        assert_eq!(
            restored_server_profile(Some("hosted:server-one"), &[profile], true),
            None
        );
    }

    #[test]
    fn first_settings_file_migrates_the_legacy_vim_default() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        let presentation = PresentationState {
            legacy_vim_mode_default: true,
            ..PresentationState::default()
        };

        let settings = load_user_settings(&store, &presentation);

        assert_eq!(settings.editor.default_mode, EditorMode::Vim);
        assert_eq!(store.load().unwrap(), settings);
    }
}
