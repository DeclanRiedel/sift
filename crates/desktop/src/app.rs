use futures::StreamExt as _;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::AsyncWriteExt as _;

const HISTORY_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ESTIMATED_PLAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ANALYZED_PLAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn system_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

use gpui::{
    div, prelude::*, px, App, Bounds, Context, Entity, IntoElement, Window, WindowBounds,
    WindowOptions,
};
use sift_api_types::{
    ConnectionProfileId, CredentialMode, RoomId, StartRunRequest, TenantId,
    UpsertConnectionProfileRequest, VcsCommitRequest, VcsDiffQuery, VcsDiscardRequest,
    VcsHunkRequest, VcsPathsRequest, VcsRevertHunkRequest, VcsUncommitRequest,
};
use sift_client_sdk::{
    Client, Error as ClientError, Ingest, OpenConnectionFromProfileRequest, RoomReplica,
    SessionTokenProvider,
};
use sift_protocol::{ConnectionId, SessionId};
use sift_workspace_ui::{
    ConnectionHealthFailure, ConnectionHealthReport, ConnectionStatus, EditorMode, ExecutorCommand,
    ExecutorEvent, PresentationState, PresentationStore, Rect, ResultState, RoomDocumentCommand,
    RoomDocumentEvent, SemanticOutcome, SemanticRequestKind, SettingsStore, UserSettings,
    WorkspaceFilesSnapshot, WorkspaceShell,
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
                id: format!(
                    "{}:{}",
                    if profile.kind == sift_workspace_ui::SavedServerKind::Ssh {
                        "ssh"
                    } else {
                        "hosted"
                    },
                    profile.id
                ),
                name: profile.name,
                base_url: profile.base_url,
                kind: if profile.kind == sift_workspace_ui::SavedServerKind::Ssh {
                    sift_workspace_ui::InstanceKind::Ssh
                } else {
                    sift_workspace_ui::InstanceKind::Hosted
                },
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
    store: Option<Arc<PresentationStore>>,
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
                reconcile_configured_root(&mut instance_roots, saved.clone());
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
            store: Some(self.presentation_store.clone()),
            settings_store: self.settings_store.clone(),
            settings: self
                .settings_store
                .load()
                .unwrap_or_else(|_| self.settings.clone()),
            runtime: self.runtime.clone(),
            server: restored_profile
                .as_ref()
                .filter(|profile| profile.kind != sift_workspace_ui::SavedServerKind::Ssh)
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

impl WindowServices {
    fn for_secondary_window(&self, state: &PresentationState) -> Self {
        let mut services = self.clone();
        services.store = None;
        services.restored_profile_id = None;
        match state.workspace.instance_id.as_deref() {
            Some(instance_id)
                if instance_id.starts_with("hosted:") || instance_id.starts_with("ssh:") =>
            {
                if let Some(profile) =
                    restored_server_profile(Some(instance_id), &services.saved_servers, false)
                {
                    if profile.kind != sift_workspace_ui::SavedServerKind::Ssh {
                        services.server = DesktopServer::remote(profile.clone(), None);
                    } else {
                        services.server = services.local_target.clone();
                    }
                    services.restored_profile_id = Some(profile.id);
                }
            }
            Some(instance_id) if instance_id.starts_with("config:") => {
                let manifest_id = instance_id.trim_start_matches("config:");
                if let Some(root) = services
                    .instance_roots
                    .iter()
                    .find(|root| root.manifest_id == manifest_id)
                {
                    if let Ok(server) = DesktopServer::configured(root.root.clone()) {
                        services.server = server;
                    }
                }
            }
            _ => services.server = services.local_target.clone(),
        }
        services
    }
}

fn reconcile_configured_root(
    roots: &mut Vec<sift_workspace_ui::SavedInstanceRoot>,
    saved: sift_workspace_ui::SavedInstanceRoot,
) {
    if saved.name == "desktop-demo" {
        roots.retain(|candidate| {
            candidate.name != saved.name || candidate.manifest_id == saved.manifest_id
        });
    }
    if let Some(index) = roots
        .iter()
        .position(|candidate| candidate.manifest_id == saved.manifest_id)
    {
        roots[index] = saved;
    } else {
        roots.push(saved);
    }
    roots.sort_by(|left, right| left.name.cmp(&right.name));
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
    let instance_id = instance_id?;
    let profile_id = instance_id
        .strip_prefix("hosted:")
        .or_else(|| instance_id.strip_prefix("ssh:"))?;
    profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned()
}

/// Window-level ownership boundary. Additional windows can each own exactly
/// one virtual workspace without adding product state to `SiftApp`.
pub struct SiftWindow {
    workspace: Entity<WorkspaceShell>,
    services: WindowServices,
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
        let retained_services = services.clone();
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
            WorkspaceShell::new(state, settings, store, Some(settings_store), window, cx)
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
            services: retained_services,
            _runtime: runtime,
        }
    }

    fn open_new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut state = self.workspace.read(cx).snapshot(cx);
        let current = window.bounds();
        let bounds = Bounds {
            origin: gpui::point(current.origin.x + px(28.), current.origin.y + px(28.)),
            size: current.size,
        };
        state.window.bounds = sift_workspace_ui::Rect {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
        };
        state.window.maximized = false;
        let services = self.services.for_secondary_window(&state);
        let platform = format!("{:?}", crate::platform::current_platform());
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(format!("Sift · {platform}").into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SiftWindow::new(state, services, window, cx)),
        ) {
            eprintln!("sift-desktop: opening a new window failed: {error}");
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

/// An opened target with separate execution and metadata lanes. Streaming a
/// query holds its driver connection until the terminal page, so catalog and
/// migration work must not share that physical connection.
struct QueryContext {
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    transaction: Option<sift_protocol::TransactionInfo>,
    metadata_connection: ConnectionId,
    /// Execution plans get their own persistent lane. Opening a physical
    /// database connection for every Explain click made a cheap estimated plan
    /// feel slower than running many queries, while sharing either lane can
    /// block behind a streaming query or corrupt SQL Server SHOWPLAN state.
    plan_connection: ConnectionId,
    /// One explain sequence at a time. SQL Server's SHOWPLAN ON/query/OFF
    /// sequence must not interleave when two tabs request plans together.
    plan_lock: Arc<tokio::sync::Mutex<()>>,
    profile_id: i64,
    connection_profile_id: Option<i64>,
    tenant_id: i64,
    /// Semantic work runs on its own task; dropping this sender ends it and
    /// releases every server document it owns with the connection.
    semantic: tokio::sync::mpsc::UnboundedSender<SemanticControl>,
}

fn spawn_notification_stream(
    context: &QueryContext,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> tokio::task::JoinHandle<()> {
    let client = context.client.clone();
    let session = context.session;
    let connection = context.metadata_connection;
    tokio::spawn(async move {
        let Ok(mut stream) = client
            .subscribe_notifications(session, connection, vec!["sift".into(), "events".into()])
            .await
        else {
            return;
        };
        while let Ok((channel, payload)) = stream.next().await {
            if events
                .send(ExecutorEvent::ServerNotification { channel, payload })
                .is_err()
            {
                return;
            }
        }
    })
}

async fn check_connection_health(context: &QueryContext) -> ConnectionHealthReport {
    let started = std::time::Instant::now();
    let failure = match context.client.health().await {
        Ok(_) => context
            .client
            .ping_connection(context.session, context.connection)
            .await
            .err()
            .map(|error| ConnectionHealthFailure::Database(format!("{error}"))),
        Err(error) => Some(ConnectionHealthFailure::Server(format!("{error}"))),
    };
    ConnectionHealthReport {
        checked_at_ms: system_epoch_millis(),
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        failure,
    }
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
    let mut health_tick = tokio::time::interval(std::time::Duration::from_secs(15));
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut active_queries: HashMap<u64, (u64, tokio::sync::mpsc::UnboundedSender<QueryControl>)> =
        HashMap::new();
    let mut active_exports: HashMap<u64, tokio::sync::oneshot::Sender<()>> = HashMap::new();
    let mut active_transfers: HashMap<u64, tokio::sync::oneshot::Sender<()>> = HashMap::new();
    let mut notification_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut repository_history_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut repository_diff_task: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            _ = health_tick.tick(), if context.is_some() && active_queries.values().all(|(_, control)| control.is_closed()) => {
                let opened = context.as_ref().expect("guarded by context");
                let report = check_connection_health(opened).await;
                if events.send(ExecutorEvent::ConnectionHealth(report)).is_err() {
                    return;
                }
                continue;
            }
            changed = targets.changed() => {
                if changed.is_err() {
                    return;
                }
                cancel_active_queries(&mut active_queries);
                active_exports.clear();
                active_transfers.clear();
                if let Some(task) = notification_task.take() {
                    task.abort();
                }
                if let Some(task) = repository_history_task.take() {
                    task.abort();
                }
                if let Some(task) = repository_diff_task.take() {
                    task.abort();
                }
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
            ExecutorCommand::LoadChangeLedger { filter } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .change_ledger(&filter)
                        .await
                        .map_err(|error| format!("loading database change ledger failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::ChangeLedgerLoaded { filter, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::Connect {
                tenant_id,
                profile_id,
                name,
            } => {
                cancel_active_queries(&mut active_queries);
                active_exports.clear();
                active_transfers.clear();
                if let Some(task) = notification_task.take() {
                    task.abort();
                }
                if let Some(previous) = context.take() {
                    let _ = previous.client.close_session(previous.session).await;
                }
                let server = targets.borrow().clone();
                match open_query_context(&server, tenant_id, profile_id, &events).await {
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
                        if events.send(load_capabilities(&opened).await).is_err() {
                            return;
                        }
                        let schema_event = load_schema(&opened).await;
                        notification_task =
                            Some(spawn_notification_stream(&opened, events.clone()));
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
            ExecutorCommand::ConnectAdHoc {
                tenant_id,
                name,
                provider_id,
                configuration,
                credentials,
            } => {
                cancel_active_queries(&mut active_queries);
                active_exports.clear();
                active_transfers.clear();
                if let Some(task) = notification_task.take() {
                    task.abort();
                }
                if let Some(previous) = context.take() {
                    let _ = previous.client.close_session(previous.session).await;
                }
                let server = targets.borrow().clone();
                match open_ad_hoc_query_context(
                    &server,
                    tenant_id,
                    provider_id,
                    configuration,
                    credentials,
                    &events,
                )
                .await
                {
                    Ok(opened) => {
                        if events
                            .send(ExecutorEvent::Connection(ConnectionStatus::Connected {
                                profile_id: 0,
                                name,
                            }))
                            .is_err()
                        {
                            return;
                        }
                        if events.send(load_capabilities(&opened).await).is_err() {
                            return;
                        }
                        let schema_event = load_schema(&opened).await;
                        notification_task =
                            Some(spawn_notification_stream(&opened, events.clone()));
                        context = Some(opened);
                        if events.send(schema_event).is_err() {
                            return;
                        }
                    }
                    Err(reason) => {
                        if events
                            .send(ExecutorEvent::Connection(ConnectionStatus::Failed {
                                profile_id: 0,
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
                active_exports.clear();
                active_transfers.clear();
                if let Some(task) = notification_task.take() {
                    task.abort();
                }
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
            ExecutorCommand::LoadSessions => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .list_sessions()
                        .await
                        .map_err(|error| format!("loading sessions failed: {error}")),
                    Err(error) => Err(error),
                };
                if events.send(ExecutorEvent::SessionsLoaded(result)).is_err() {
                    return;
                }
            }
            ExecutorCommand::CloseSession { session_id } => {
                let active_connection_closed = context
                    .as_ref()
                    .is_some_and(|opened| opened.session == session_id);
                if active_connection_closed {
                    cancel_active_queries(&mut active_queries);
                    active_exports.clear();
                    active_transfers.clear();
                    if let Some(task) = notification_task.take() {
                        task.abort();
                    }
                    context = None;
                }
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .close_session(session_id)
                        .await
                        .map_err(|error| format!("closing session failed: {error}")),
                    Err(error) => Err(error),
                };
                if active_connection_closed {
                    let _ = events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected));
                }
                if events
                    .send(ExecutorEvent::SessionResourceClosed {
                        result,
                        active_connection_closed,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CloseConnection {
                session_id,
                connection_id,
            } => {
                let active_connection_closed = context.as_ref().is_some_and(|opened| {
                    opened.session == session_id
                        && [
                            opened.connection,
                            opened.metadata_connection,
                            opened.plan_connection,
                        ]
                        .contains(&connection_id)
                });
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) if active_connection_closed => {
                        cancel_active_queries(&mut active_queries);
                        active_exports.clear();
                        active_transfers.clear();
                        if let Some(task) = notification_task.take() {
                            task.abort();
                        }
                        context = None;
                        client
                            .close_session(session_id)
                            .await
                            .map_err(|error| format!("closing active session failed: {error}"))
                    }
                    Ok(client) => client
                        .close_connection(session_id, connection_id)
                        .await
                        .map_err(|error| format!("closing connection failed: {error}")),
                    Err(error) => Err(error),
                };
                if active_connection_closed {
                    let _ = events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected));
                }
                if events
                    .send(ExecutorEvent::SessionResourceClosed {
                        result,
                        active_connection_closed,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DisconnectConnectionProfile { profile_id } => {
                let active_connection_closed = context
                    .as_ref()
                    .is_some_and(|opened| opened.profile_id == profile_id);
                if active_connection_closed {
                    cancel_active_queries(&mut active_queries);
                    active_exports.clear();
                    active_transfers.clear();
                    if let Some(task) = notification_task.take() {
                        task.abort();
                    }
                    context = None;
                }
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .disconnect_connection_profile(ConnectionProfileId(profile_id))
                        .await
                        .map(|response| response.disconnected as usize)
                        .map_err(|error| format!("disconnecting profile failed: {error}")),
                    Err(error) => Err(error),
                };
                if active_connection_closed {
                    let _ = events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected));
                }
                if events
                    .send(ExecutorEvent::ConnectionProfileDisconnected { profile_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CheckConnectionHealth => {
                active_queries.retain(|_, (_, control)| !control.is_closed());
                if active_queries.is_empty() {
                    if let Some(opened) = context.as_ref() {
                        let report = check_connection_health(opened).await;
                        if events
                            .send(ExecutorEvent::ConnectionHealth(report))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            ExecutorCommand::BeginTransaction => {
                let result = match context.as_mut() {
                    Some(opened) if opened.transaction.is_none() => opened
                        .client
                        .begin_transaction(
                            opened.session,
                            opened.connection,
                            sift_protocol::TxMode::default(),
                        )
                        .await
                        .map(|transaction| {
                            opened.transaction = Some(transaction.clone());
                            Some(transaction)
                        })
                        .map_err(|error| format!("beginning transaction failed: {error}")),
                    Some(_) => Err("A transaction is already open".into()),
                    None => Err("Connect to a database before beginning a transaction".into()),
                };
                if events
                    .send(ExecutorEvent::TransactionChanged(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CommitTransaction | ExecutorCommand::RollbackTransaction => {
                let commit = matches!(command, ExecutorCommand::CommitTransaction);
                active_queries.retain(|_, (_, control)| !control.is_closed());
                let result = if commit && !active_queries.is_empty() {
                    Err("Wait for running queries before committing the transaction".into())
                } else {
                    match context.as_mut() {
                        Some(opened) => match opened.transaction.clone() {
                            Some(transaction) => {
                                let ended = if commit {
                                    opened
                                        .client
                                        .commit_transaction(
                                            opened.session,
                                            transaction.connection,
                                            transaction.tx_id,
                                        )
                                        .await
                                } else {
                                    opened
                                        .client
                                        .rollback_transaction(
                                            opened.session,
                                            transaction.connection,
                                            transaction.tx_id,
                                        )
                                        .await
                                };
                                ended
                                    .map(|()| {
                                        opened.transaction = None;
                                        None
                                    })
                                    .map_err(|error| format!("ending transaction failed: {error}"))
                            }
                            None => Err("No transaction is open".into()),
                        },
                        None => Err("No database connection is open".into()),
                    }
                };
                if events
                    .send(ExecutorEvent::TransactionChanged(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateSavepoint { ref name }
            | ExecutorCommand::RollbackToSavepoint { ref name }
            | ExecutorCommand::ReleaseSavepoint { ref name } => {
                let action = match &command {
                    ExecutorCommand::CreateSavepoint { .. } => "created",
                    ExecutorCommand::RollbackToSavepoint { .. } => "rolled back",
                    ExecutorCommand::ReleaseSavepoint { .. } => "released",
                    _ => unreachable!(),
                };
                let result = match context
                    .as_ref()
                    .and_then(|opened| opened.transaction.as_ref().map(|tx| (opened, tx)))
                {
                    Some((opened, transaction)) => match action {
                        "created" => {
                            opened
                                .client
                                .create_savepoint(
                                    opened.session,
                                    transaction.connection,
                                    transaction.tx_id,
                                    name.clone(),
                                )
                                .await
                        }
                        "rolled back" => {
                            opened
                                .client
                                .rollback_to_savepoint(
                                    opened.session,
                                    transaction.connection,
                                    transaction.tx_id,
                                    name.clone(),
                                )
                                .await
                        }
                        _ => {
                            opened
                                .client
                                .release_savepoint(
                                    opened.session,
                                    transaction.connection,
                                    transaction.tx_id,
                                    name.clone(),
                                )
                                .await
                        }
                    }
                    .map_err(|error| format!("savepoint operation failed: {error}")),
                    None => Err("No transaction is open".into()),
                };
                if events
                    .send(ExecutorEvent::SavepointChanged {
                        action,
                        name: name.clone(),
                        result,
                    })
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
                params,
                transform,
                source,
                variable_context,
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
                let transaction = opened.transaction.clone();
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    run_streamed_query(
                        QueryRun {
                            client,
                            session,
                            connection,
                            transaction,
                            item_id,
                            execution_id,
                            sql,
                            params,
                            transform,
                            source,
                            variable_context,
                        },
                        control_receiver,
                        events,
                    )
                    .await;
                }));
            }
            ExecutorCommand::Explain {
                item_id,
                request_id,
                tenant_id: _,
                profile_id,
                sql,
                analyze,
            } => {
                let Some(opened) = context
                    .as_ref()
                    .filter(|opened| opened.profile_id == profile_id)
                else {
                    if events
                        .send(ExecutorEvent::ExplainFinished {
                            item_id,
                            request_id,
                            response: Err(
                                "Connect to this database before requesting a plan".into()
                            ),
                        })
                        .is_err()
                    {
                        return;
                    }
                    continue;
                };
                let client = opened.client.clone();
                let session = opened.session;
                let connection = opened.plan_connection;
                let plan_lock = opened.plan_lock.clone();
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    let deadline = if analyze {
                        ANALYZED_PLAN_TIMEOUT
                    } else {
                        ESTIMATED_PLAN_TIMEOUT
                    };
                    let explain = async {
                        let _plan_guard = plan_lock.lock().await;
                        client
                            .explain(
                                session,
                                connection,
                                sift_protocol::ExplainRequest {
                                    connection,
                                    sql,
                                    params: Vec::new(),
                                    analyze,
                                },
                            )
                            .await
                            .map(Box::new)
                            .map_err(|error| format!("explaining query failed: {error}"))
                    };
                    let response = tokio::time::timeout(deadline, explain)
                        .await
                        .unwrap_or_else(|_| {
                            Err(if analyze {
                                "Analyzed plan timed out after 120 seconds"
                            } else {
                                "Estimated plan timed out after 10 seconds"
                            }
                            .into())
                        });
                    let _ = events.send(ExecutorEvent::ExplainFinished {
                        item_id,
                        request_id,
                        response,
                    });
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
                vault_id,
                name,
                provider_id,
                configuration,
                credentials,
                credential_mode,
                tags,
            } => {
                if let Some(previous) = context.take() {
                    if let Some(task) = notification_task.take() {
                        task.abort();
                    }
                    let _ = previous.client.close_session(previous.session).await;
                }
                let server = targets.borrow().clone();
                let result = create_connection_profile(
                    &server,
                    UpsertConnectionProfileRequest {
                        tenant_id,
                        vault_id,
                        name,
                        provider_id,
                        configuration,
                        credentials,
                        credential_mode,
                        tags,
                    },
                )
                .await;
                match result {
                    Ok(entry) => {
                        let connection_error =
                            match open_query_context(&server, tenant_id, entry.id, &events).await {
                                Ok(opened) => {
                                    let _ = events.send(ExecutorEvent::Connection(
                                        ConnectionStatus::Connected {
                                            profile_id: entry.id,
                                            name: entry.name.clone(),
                                        },
                                    ));
                                    let _ = events.send(load_capabilities(&opened).await);
                                    let schema_event = load_schema(&opened).await;
                                    notification_task =
                                        Some(spawn_notification_stream(&opened, events.clone()));
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
            ExecutorCommand::LoadConnectionProfile {
                tenant_id,
                profile_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .connection_profiles(TenantId(tenant_id))
                        .await
                        .map_err(|error| format!("loading connection profile failed: {error}"))
                        .and_then(|profiles| {
                            profiles
                                .into_iter()
                                .find(|profile| profile.id.0 == profile_id)
                                .map(Box::new)
                                .ok_or_else(|| "Connection profile no longer exists".into())
                        }),
                    Err(error) => Err(error),
                };
                if events.send(ExecutorEvent::ProfileLoaded(result)).is_err() {
                    return;
                }
            }
            ExecutorCommand::TestConnectionProfile {
                tenant_id,
                provider_id,
                configuration,
                credentials,
            } => {
                let server = targets.borrow().clone();
                let result = test_connection_profile(
                    &server,
                    tenant_id,
                    provider_id,
                    configuration,
                    credentials,
                    &events,
                )
                .await;
                if events.send(ExecutorEvent::ProfileTested(result)).is_err() {
                    return;
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
                        if let Some(task) = notification_task.take() {
                            task.abort();
                        }
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
            ExecutorCommand::LoadDatabaseProcesses => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .list_processes(opened.session, opened.metadata_connection)
                        .await
                        .map_err(|error| format!("loading database activity failed: {error}")),
                    None => Err("Connect before loading database activity".into()),
                };
                if events
                    .send(ExecutorEvent::DatabaseProcessesLoaded(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRoomMembers { room_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .room_members(sift_api_types::RoomId(room_id))
                        .await
                        .map_err(|error| format!("loading room members failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RoomMembersLoaded { room_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RemoveRoomMember {
                room_id,
                principal_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .remove_room_member(sift_api_types::RoomId(room_id), principal_id)
                        .await
                        .map_err(|error| format!("removing room member failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RoomMemberRemoved {
                        room_id,
                        principal_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadVaults { tenant_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .vaults(sift_api_types::TenantId(tenant_id))
                        .await
                        .map_err(|error| format!("loading vaults failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultsLoaded { tenant_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadVaultItems { vault_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .vault_items(sift_api_types::VaultId(vault_id))
                        .await
                        .map_err(|error| format!("loading vault items failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemsLoaded { vault_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadVaultItemVersions { item_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .vault_item_versions(sift_api_types::VaultItemId(item_id))
                        .await
                        .map_err(|error| format!("loading vault history failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemVersionsLoaded { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadVaultGrants { vault_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .vault_grants(sift_api_types::VaultId(vault_id))
                        .await
                        .map_err(|error| format!("loading vault access failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultGrantsLoaded { vault_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RevealVaultItem { item_id, password } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let lease = if let Some(password) = password {
                            match client
                                .step_up_vault_reveal(
                                    sift_api_types::VaultItemId(item_id),
                                    sift_api_types::VaultRevealStepUpRequest { password },
                                )
                                .await
                            {
                                Ok(step_up) => Some(step_up.lease),
                                Err(error) => {
                                    let _ = events.send(ExecutorEvent::VaultItemRevealed {
                                        item_id,
                                        result: Err(format!("vault step-up failed: {error}")),
                                    });
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        client
                            .reveal_vault_item(sift_api_types::VaultItemId(item_id), lease)
                            .await
                            .map_err(|error| format!("revealing vault item failed: {error}"))
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemRevealed { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateTeamVault { tenant_id, name } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .create_vault(sift_api_types::CreateVaultRequest {
                            tenant_id,
                            scope: sift_protocol::VaultScope::Team,
                            name,
                        })
                        .await
                        .map_err(|error| format!("creating team vault failed: {error}")),
                    Err(error) => Err(error),
                };
                if events.send(ExecutorEvent::VaultCreated(result)).is_err() {
                    return;
                }
            }
            ExecutorCommand::UpdateVault { vault_id, request } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .update_vault(sift_api_types::VaultId(vault_id), request)
                        .await
                        .map(Some)
                        .map_err(|error| format!("renaming vault failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultMutated {
                        vault_id,
                        action: "Renamed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteVault {
                vault_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_vault(sift_api_types::VaultId(vault_id), expected_revision)
                        .await
                        .map(|()| None)
                        .map_err(|error| format!("deleting vault failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultMutated {
                        vault_id,
                        action: "Deleted",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateVaultItem { vault_id, request } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .create_vault_item(sift_api_types::VaultId(vault_id), request)
                        .await
                        .map_err(|error| format!("storing vault item failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemCreated { vault_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::UpdateVaultItem { item_id, request } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .update_vault_item(sift_api_types::VaultItemId(item_id), request)
                        .await
                        .map(Some)
                        .map_err(|error| format!("updating vault item failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemMutated {
                        item_id,
                        action: "Updated",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetVaultItemSecret { item_id, request } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .set_vault_item_secret(sift_api_types::VaultItemId(item_id), request)
                        .await
                        .map(Some)
                        .map_err(|error| format!("rotating vault secret failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemMutated {
                        item_id,
                        action: "Rotated",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ClearVaultItemSecret {
                item_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .clear_vault_item_secret(
                            sift_api_types::VaultItemId(item_id),
                            expected_revision,
                        )
                        .await
                        .map(Some)
                        .map_err(|error| format!("clearing vault secret failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemMutated {
                        item_id,
                        action: "Cleared",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RestoreVaultItem { item_id, request } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .restore_vault_item(sift_api_types::VaultItemId(item_id), request)
                        .await
                        .map(Some)
                        .map_err(|error| format!("restoring vault item failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemMutated {
                        item_id,
                        action: "Restored",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteVaultItem {
                vault_id: _,
                item_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_vault_item(sift_api_types::VaultItemId(item_id), expected_revision)
                        .await
                        .map(|()| None)
                        .map_err(|error| format!("deleting vault item failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemMutated {
                        item_id,
                        action: "Deleted",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::TestVaultItem { item_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .test_vault_item(sift_api_types::VaultItemId(item_id))
                        .await
                        .map(|()| None)
                        .map_err(|error| format!("testing vault connection failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultItemMutated {
                        item_id,
                        action: "Connection test passed for",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetVaultGrant {
                vault_id,
                principal_id,
                request,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .set_vault_grant(
                            sift_api_types::VaultId(vault_id),
                            sift_api_types::PrincipalId(principal_id),
                            request,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("updating vault access failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultGrantMutated { vault_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteVaultGrant {
                vault_id,
                principal_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_vault_grant(
                            sift_api_types::VaultId(vault_id),
                            sift_api_types::PrincipalId(principal_id),
                            expected_revision,
                        )
                        .await
                        .map_err(|error| format!("revoking vault access failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultGrantMutated { vault_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ShareVault {
                vault_id,
                principal_ids,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        match client.vault_grants(sift_api_types::VaultId(vault_id)).await {
                            Ok(grants) => {
                                let mut shared = 0usize;
                                let mut failure = None;
                                for principal_id in principal_ids {
                                    let existing = grants
                                        .iter()
                                        .find(|grant| grant.principal_id.0 == principal_id);
                                    let mut capabilities = existing
                                        .map(|grant| grant.capabilities)
                                        .unwrap_or_default();
                                    capabilities.inspect = true;
                                    capabilities.use_secret = true;
                                    let request = sift_api_types::SetVaultGrantRequest {
                                        expected_revision: existing.map(|grant| grant.revision),
                                        capabilities,
                                    };
                                    if let Err(error) = client
                                        .set_vault_grant(
                                            sift_api_types::VaultId(vault_id),
                                            sift_api_types::PrincipalId(principal_id),
                                            request,
                                        )
                                        .await
                                    {
                                        failure = Some(format!("sharing vault failed: {error}"));
                                        break;
                                    }
                                    shared += 1;
                                }
                                failure.map_or(Ok(shared), Err)
                            }
                            Err(error) => Err(format!("loading vault grants failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::VaultShared { vault_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryStatus {
                workspace_id,
                request_id,
            } => {
                let server = targets.borrow().clone();
                let (binding, result) = match server.client().await {
                    Ok(client) => match client
                        .workspace_repository(sift_protocol::WorkspaceId(workspace_id))
                        .await
                    {
                        Ok(Some(binding)) => {
                            let result = client
                                .repository_status(binding.id)
                                .await
                                .map(Some)
                                .map_err(|error| {
                                    format!("loading repository status failed: {error}")
                                });
                            (Some(binding), result)
                        }
                        Ok(None) => (None, Ok(None)),
                        Err(error) => (
                            None,
                            Err(format!("loading repository binding failed: {error}")),
                        ),
                    },
                    Err(error) => (None, Err(error)),
                };
                if events
                    .send(ExecutorEvent::RepositoryStatusLoaded {
                        workspace_id,
                        request_id,
                        binding,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::BindWorkspaceRepository {
                workspace_id,
                root_handle,
                initialize,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let workspace = sift_protocol::WorkspaceId(workspace_id);
                        let projection = match client.workspace_projection(workspace).await {
                            Ok(Some(projection)) => Ok(projection),
                            Ok(None) => {
                                client
                                    .bind_workspace_projection(
                                        workspace,
                                        sift_api_types::BindWorkspaceProjectionRequest {
                                            root_handle,
                                            mode: sift_protocol::ProjectionMode::ReadWrite,
                                        },
                                    )
                                    .await
                            }
                            Err(error) => Err(error),
                        };
                        match projection {
                            Ok(projection) => client
                                .bind_workspace_repository(
                                    workspace,
                                    sift_api_types::BindRepositoryRequest {
                                        projection_id: projection.id,
                                        initialize,
                                    },
                                )
                                .await
                                .map_err(|error| format!("binding repository failed: {error}")),
                            Err(error) => Err(format!("binding projection failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositorySetupFinished {
                        workspace_id,
                        action: if initialize {
                            "Repository initialized"
                        } else {
                            "Repository bound"
                        },
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CloneWorkspaceRepository {
                workspace_id,
                root_handle,
                url,
                username,
                password,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .clone_workspace_repository(
                            sift_protocol::WorkspaceId(workspace_id),
                            sift_api_types::CloneWorkspaceRepositoryRequest {
                                root_handle,
                                url,
                                username: sift_protocol::RedactedString(username),
                                password: sift_protocol::RedactedString(password),
                            },
                        )
                        .await
                        .map_err(|error| format!("cloning repository failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositorySetupFinished {
                        workspace_id,
                        action: "Repository cloned",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryRemotes {
                workspace_id,
                binding_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repository_remotes(sift_protocol::RepositoryBindingId(binding_id))
                        .await
                        .map_err(|error| format!("loading remotes failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryRemotesLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryHosting {
                workspace_id,
                binding_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repository_hosting(
                            sift_protocol::RepositoryBindingId(binding_id),
                            None,
                            None,
                        )
                        .await
                        .map_err(|error| format!("loading hosting state failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHostingLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadHostingRepositories {
                workspace_id,
                binding_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .hosting_repositories(sift_protocol::RepositoryBindingId(binding_id), None)
                        .await
                        .map_err(|error| format!("loading hosted repositories failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::HostingRepositoriesLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetHostingCredential {
                workspace_id,
                binding_id,
                expected_revision,
                token,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .set_hosting_credential(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_protocol::SetHostingCredentialRequest {
                                expected_revision,
                                token: sift_protocol::RedactedString(token),
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("saving hosting credential failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHostingFinished {
                        workspace_id,
                        action: "Hosting credential saved",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteHostingCredential {
                workspace_id,
                binding_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_hosting_credential(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::ExpectedRepositoryRevisionRequest { expected_revision },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("removing hosting credential failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHostingFinished {
                        workspace_id,
                        action: "Hosting credential removed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateHostingPullRequest {
                workspace_id,
                binding_id,
                expected_revision,
                title,
                head_branch,
                base_branch,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .create_hosting_pull_request(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_protocol::CreateHostingPullRequestRequest {
                                expected_revision,
                                title,
                                body: None,
                                head_branch,
                                base_branch,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("creating pull request failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHostingFinished {
                        workspace_id,
                        action: "Pull request created",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::AddRepositoryRemote {
                workspace_id,
                binding_id,
                expected_revision,
                name,
                url,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let request = sift_api_types::VcsRemoteMutationRequest {
                            expected_revision,
                            name,
                            url,
                        };
                        client
                            .add_repository_remote(
                                sift_protocol::RepositoryBindingId(binding_id),
                                request,
                            )
                            .await
                            .map_err(|error| format!("saving remote failed: {error}"))
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConfigurationFinished {
                        workspace_id,
                        action: "Remote added",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::UpdateRepositoryRemote {
                workspace_id,
                binding_id,
                expected_revision,
                old_name,
                name,
                url,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        let revision = if let Some(old_name) = old_name.filter(|old| old != &name) {
                            match client
                                .rename_repository_remote(
                                    binding,
                                    sift_api_types::VcsRemoteRenameRequest {
                                        expected_revision,
                                        old_name,
                                        new_name: name.clone(),
                                    },
                                )
                                .await
                            {
                                Ok(updated) => Ok(updated.revision),
                                Err(error) => Err(format!("renaming remote failed: {error}")),
                            }
                        } else {
                            Ok(expected_revision)
                        };
                        match revision {
                            Ok(expected_revision) => client
                                .update_repository_remote(
                                    binding,
                                    sift_api_types::VcsRemoteMutationRequest {
                                        expected_revision,
                                        name,
                                        url,
                                    },
                                )
                                .await
                                .map_err(|error| format!("saving remote failed: {error}")),
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConfigurationFinished {
                        workspace_id,
                        action: "Remote updated",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RemoveRepositoryRemote {
                workspace_id,
                binding_id,
                expected_revision,
                name,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .remove_repository_remote(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsRemoteDeleteRequest {
                                expected_revision,
                                name,
                            },
                        )
                        .await
                        .map_err(|error| format!("removing remote failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConfigurationFinished {
                        workspace_id,
                        action: "Remote removed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetRepositoryCredential {
                workspace_id,
                binding_id,
                expected_revision,
                username,
                password,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .set_repository_credential(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::SetVcsCredentialRequest {
                                expected_revision,
                                username: sift_protocol::RedactedString(username),
                                password: sift_protocol::RedactedString(password),
                            },
                        )
                        .await
                        .map_err(|error| format!("saving credential failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConfigurationFinished {
                        workspace_id,
                        action: "Credential saved",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::TestRepositoryCredential {
                workspace_id,
                binding_id,
                expected_revision,
                remote,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .test_repository_credential(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsCredentialTestRequest {
                                expected_revision,
                                remote,
                            },
                        )
                        .await
                        .map_err(|error| format!("testing credential failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryCredentialTested {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RemoveRepositoryCredential {
                workspace_id,
                binding_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_repository_credential(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::ExpectedRepositoryRevisionRequest { expected_revision },
                        )
                        .await
                        .map_err(|error| format!("removing credential failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConfigurationFinished {
                        workspace_id,
                        action: "Credential removed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::FetchRepository {
                workspace_id,
                binding_id,
                expected_revision,
                remote,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .fetch_repository(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsRemoteRequest {
                                expected_revision,
                                remote,
                                branch: None,
                            },
                        )
                        .await
                        .map_err(|error| format!("fetch failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryRemoteFinished {
                        workspace_id,
                        action: "Fetch completed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::PushRepository {
                workspace_id,
                binding_id,
                expected_revision,
                remote,
                branch,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .push_repository(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsRemoteRequest {
                                expected_revision,
                                remote,
                                branch,
                            },
                        )
                        .await
                        .map_err(|error| format!("push failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryRemoteFinished {
                        workspace_id,
                        action: "Push completed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryBranches {
                workspace_id,
                binding_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repository_branches(sift_protocol::RepositoryBindingId(binding_id))
                        .await
                        .map_err(|error| format!("loading branches failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryBranchesLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryHistory {
                workspace_id,
                binding_id,
                request_id,
                cursor,
                query,
                append,
            } => {
                let server = targets.borrow().clone();
                if let Some(task) = repository_history_task.take() {
                    task.abort();
                }
                let events = events.clone();
                repository_history_task = Some(tokio::spawn(async move {
                    let result = match server.client().await {
                        Ok(client) => client
                            .repository_history(
                                sift_protocol::RepositoryBindingId(binding_id),
                                sift_api_types::VcsHistoryQuery {
                                    cursor,
                                    limit: 80,
                                    query,
                                },
                            )
                            .await
                            .map_err(|error| format!("loading repository history failed: {error}")),
                        Err(error) => Err(error),
                    };
                    let _ = events.send(ExecutorEvent::RepositoryHistoryLoaded {
                        workspace_id,
                        request_id,
                        append,
                        result,
                    });
                }));
            }
            ExecutorCommand::LoadRepositoryCommit {
                workspace_id,
                binding_id,
                oid,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repository_commit(sift_protocol::RepositoryBindingId(binding_id), &oid)
                        .await
                        .map_err(|error| format!("loading commit failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryCommitLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryHistoricalFile {
                workspace_id,
                binding_id,
                oid,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repository_historical_file(
                            sift_protocol::RepositoryBindingId(binding_id),
                            &oid,
                            path,
                        )
                        .await
                        .map_err(|error| format!("loading historical file failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHistoricalFileLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CompareRepositoryCommits {
                workspace_id,
                binding_id,
                base,
                target,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .compare_repository_commits(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsCompareQuery { base, target },
                        )
                        .await
                        .map_err(|error| format!("comparing commits failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryComparisonLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RestoreRepositoryHistoricalFile {
                workspace_id,
                binding_id,
                expected_revision,
                oid,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .restore_repository_historical_file(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsRestoreHistoricalFileRequest {
                                expected_revision,
                                commit: oid,
                                path,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("restoring historical file failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHistoryMutationFinished {
                        workspace_id,
                        action: "Historical file restored",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RevertRepositoryCommit {
                workspace_id,
                binding_id,
                expected_revision,
                oid,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .revert_repository_commit(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsRevertCommitRequest {
                                expected_revision,
                                commit: oid,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("preparing commit revert failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHistoryMutationFinished {
                        workspace_id,
                        action: "Commit revert prepared for review",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateRepositoryBranch {
                workspace_id,
                binding_id,
                expected_revision,
                name,
                start,
                checkpoint_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .create_repository_branch(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsCreateBranchRequest {
                                expected_revision,
                                name,
                                start,
                                checkpoint_id,
                            },
                        )
                        .await
                        .map_err(|error| format!("creating branch failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryBranchChanged {
                        workspace_id,
                        action: "Branch created",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SwitchRepositoryBranch {
                workspace_id,
                binding_id,
                expected_revision,
                target,
                detached,
                checkpoint_changes,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .switch_repository_branch(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsSwitchBranchRequest {
                                expected_revision,
                                target,
                                detached,
                                checkpoint_changes,
                            },
                        )
                        .await
                        .map_err(|error| format!("switching branch failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryBranchChanged {
                        workspace_id,
                        action: "Branch switched",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RenameRepositoryBranch {
                workspace_id,
                binding_id,
                expected_revision,
                old,
                new,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .rename_repository_branch(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsRenameBranchRequest {
                                expected_revision,
                                old,
                                new,
                            },
                        )
                        .await
                        .map_err(|error| format!("renaming branch failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryBranchChanged {
                        workspace_id,
                        action: "Branch renamed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteRepositoryBranch {
                workspace_id,
                binding_id,
                expected_revision,
                name,
                force,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_repository_branch(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsDeleteBranchRequest {
                                expected_revision,
                                name,
                                force,
                                confirm_unmerged: force,
                            },
                        )
                        .await
                        .map_err(|error| format!("deleting branch failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryBranchChanged {
                        workspace_id,
                        action: "Branch deleted",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetRepositoryUpstream {
                workspace_id,
                binding_id,
                expected_revision,
                branch,
                upstream,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .set_repository_upstream(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsSetUpstreamRequest {
                                expected_revision,
                                branch,
                                upstream,
                            },
                        )
                        .await
                        .map_err(|error| format!("updating branch upstream failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryBranchChanged {
                        workspace_id,
                        action: "Branch upstream updated",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryConflict {
                workspace_id,
                binding_id,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repository_conflict(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsConflictQuery { path },
                        )
                        .await
                        .map_err(|error| format!("loading conflict failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConflictLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ResolveRepositoryConflict {
                workspace_id,
                binding_id,
                expected_revision,
                path,
                region_id,
                resolution,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .resolve_repository_conflict(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsResolveConflictRequest {
                                expected_revision,
                                path,
                                region_id,
                                resolution,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("resolving conflict failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConflictMutationFinished {
                        workspace_id,
                        action: "Conflict resolved",
                        manual_path: None,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::BeginManualRepositoryConflict {
                workspace_id,
                binding_id,
                expected_revision,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .begin_repository_conflict_resolution(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsBeginConflictResolutionRequest {
                                expected_revision,
                                path: path.clone(),
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("checkpointing conflict failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConflictMutationFinished {
                        workspace_id,
                        action: "Conflict checkpoint created",
                        manual_path: Some(path),
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::MarkRepositoryConflictResolved {
                workspace_id,
                binding_id,
                expected_revision,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .mark_repository_conflict_resolved(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::VcsMarkConflictResolvedRequest {
                                expected_revision,
                                path,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("marking conflict resolved failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConflictMutationFinished {
                        workspace_id,
                        action: "Conflict marked resolved",
                        manual_path: None,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::MutateRepositoryOperation {
                workspace_id,
                binding_id,
                expected_revision,
                kind,
                abort,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let request = sift_api_types::VcsRepositoryOperationRequest {
                            expected_revision,
                            kind,
                        };
                        if abort {
                            client
                                .abort_repository_operation(
                                    sift_protocol::RepositoryBindingId(binding_id),
                                    request,
                                )
                                .await
                        } else {
                            client
                                .continue_repository_operation(
                                    sift_protocol::RepositoryBindingId(binding_id),
                                    request,
                                )
                                .await
                        }
                        .map(|_| ())
                        .map_err(|error| format!("updating repository operation failed: {error}"))
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConflictMutationFinished {
                        workspace_id,
                        action: if abort {
                            "Repository operation aborted"
                        } else {
                            "Repository operation continued"
                        },
                        manual_path: None,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RepairRepositoryBinding {
                workspace_id,
                binding_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .repair_repository_binding(
                            sift_protocol::RepositoryBindingId(binding_id),
                            sift_api_types::ExpectedRepositoryRevisionRequest { expected_revision },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("repairing repository binding failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryConflictMutationFinished {
                        workspace_id,
                        action: "Repository binding repaired",
                        manual_path: None,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadWorkspaceFiles {
                workspace_id,
                request_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let workspace = sift_protocol::WorkspaceId(workspace_id);
                        match client.workspace_nodes(workspace).await {
                            Ok(tree) => match client.workspace_projection(workspace).await {
                                Ok(projection) => {
                                    let reconcile_plan = match projection.as_ref() {
                                        Some(binding) => client
                                            .plan_workspace_projection(binding.id)
                                            .await
                                            .map(Some)
                                            .map_err(|error| {
                                                format!(
                                                    "planning workspace projection failed: {error}"
                                                )
                                            }),
                                        None => Ok(None),
                                    };
                                    match reconcile_plan {
                                        Ok(reconcile_plan) => client
                                            .workspace_checkpoints(workspace, None, 100)
                                            .await
                                            .map(|checkpoints| WorkspaceFilesSnapshot {
                                                tree,
                                                projection,
                                                reconcile_plan,
                                                checkpoints,
                                            })
                                            .map_err(|error| {
                                                format!(
                                                    "loading workspace checkpoints failed: {error}"
                                                )
                                            }),
                                        Err(error) => Err(error),
                                    }
                                }
                                Err(error) => {
                                    Err(format!("loading workspace projection failed: {error}"))
                                }
                            },
                            Err(error) => Err(format!("loading workspace files failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceFilesLoaded {
                        workspace_id,
                        request_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateWorkspaceNode {
                workspace_id,
                expected_revision,
                parent_id,
                path,
                kind,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .create_workspace_node(
                            sift_protocol::WorkspaceId(workspace_id),
                            sift_api_types::CreateWorkspaceNodeRequest {
                                expected_workspace_revision: expected_revision,
                                parent_id,
                                path,
                                kind,
                                initial_text: (kind
                                    == sift_protocol::WorkspaceNodeKind::SqlDocument)
                                    .then(String::new),
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("creating workspace node failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceMutationFinished {
                        workspace_id,
                        action: "Created workspace node",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::MoveWorkspaceNode {
                workspace_id,
                node_id,
                expected_revision,
                parent_id,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .move_workspace_node(
                            node_id,
                            sift_api_types::MoveWorkspaceNodeRequest {
                                expected_workspace_revision: expected_revision,
                                parent_id,
                                path,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("moving workspace node failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceMutationFinished {
                        workspace_id,
                        action: "Moved workspace node",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteWorkspaceNode {
                workspace_id,
                node_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_workspace_node(
                            node_id,
                            sift_api_types::ExpectedWorkspaceRevisionRequest { expected_revision },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("deleting workspace node failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceMutationFinished {
                        workspace_id,
                        action: "Deleted workspace node",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateWorkspaceCheckpoint {
                workspace_id,
                expected_revision,
                name,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .create_workspace_checkpoint(
                            sift_protocol::WorkspaceId(workspace_id),
                            sift_api_types::CreateWorkspaceCheckpointRequest {
                                expected_workspace_revision: expected_revision,
                                reason: sift_protocol::WorkspaceCheckpointReason::Named,
                                name: Some(name),
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("creating workspace checkpoint failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceMutationFinished {
                        workspace_id,
                        action: "Created workspace checkpoint",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RestoreWorkspaceCheckpoint {
                workspace_id,
                checkpoint_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .restore_workspace_checkpoint(
                            checkpoint_id,
                            sift_api_types::RestoreWorkspaceCheckpointRequest {
                                expected_workspace_revision: expected_revision,
                            },
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("restoring workspace checkpoint failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceMutationFinished {
                        workspace_id,
                        action: "Restored workspace checkpoint as new head",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ApplyWorkspaceProjection {
                workspace_id,
                binding_id,
                request,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .apply_workspace_projection(binding_id, request)
                        .await
                        .map(|_| ())
                        .map_err(|error| {
                            format!("reconciling workspace projection failed: {error}")
                        }),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::WorkspaceMutationFinished {
                        workspace_id,
                        action: "Reconciled workspace projection",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetRepositoryPathsStaged {
                workspace_id,
                binding_id,
                expected_revision,
                request_id,
                paths,
                staged,
            } => {
                let server = targets.borrow().clone();
                let (binding, result) = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        let request = VcsPathsRequest {
                            expected_revision,
                            paths,
                        };
                        let changed = if staged {
                            client.stage_repository_paths(binding, request).await
                        } else {
                            client.unstage_repository_paths(binding, request).await
                        };
                        match changed {
                            Ok(binding) => {
                                let result = client
                                    .repository_status(binding.id)
                                    .await
                                    .map(Some)
                                    .map_err(|error| {
                                        format!("refreshing repository status failed: {error}")
                                    });
                                (Some(binding), result)
                            }
                            Err(error) => {
                                (None, Err(format!("updating staged paths failed: {error}")))
                            }
                        }
                    }
                    Err(error) => (None, Err(error)),
                };
                if events
                    .send(ExecutorEvent::RepositoryStatusLoaded {
                        workspace_id,
                        request_id,
                        binding,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CommitRepository {
                workspace_id,
                binding_id,
                expected_revision,
                request_id,
                message,
                author_name,
                author_email,
                amend,
                expected_head,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        let request = VcsCommitRequest {
                            expected_revision,
                            expected_head,
                            message,
                            author_name,
                            author_email,
                        };
                        let committed = if amend {
                            client.amend_repository(binding, request).await
                        } else {
                            client.commit_repository(binding, request).await
                        };
                        match committed {
                            Ok(commit) => client
                                .repository_status(binding)
                                .await
                                .map(|status| (commit, Some(status)))
                                .map_err(|error| {
                                    format!("refreshing repository status failed: {error}")
                                }),
                            Err(error) => Err(format!("committing repository failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryCommitted {
                        workspace_id,
                        request_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::UncommitRepository {
                workspace_id,
                binding_id,
                expected_revision,
                request_id,
                expected_head,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        match client
                            .uncommit_repository(
                                binding,
                                VcsUncommitRequest {
                                    expected_revision,
                                    expected_head,
                                },
                            )
                            .await
                        {
                            Ok(mutation) => client
                                .repository_status(binding)
                                .await
                                .map(|status| (mutation, Some(status)))
                                .map_err(|error| {
                                    format!("refreshing repository status failed: {error}")
                                }),
                            Err(error) => Err(format!("uncommitting repository failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryUncommitted {
                        workspace_id,
                        request_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadRepositoryDiff {
                workspace_id,
                binding_id,
                request_id,
                side,
                path,
            } => {
                let server = targets.borrow().clone();
                if let Some(task) = repository_diff_task.take() {
                    task.abort();
                }
                let events = events.clone();
                repository_diff_task = Some(tokio::spawn(async move {
                    let result = match server.client().await {
                        Ok(client) => client
                            .repository_diff(
                                sift_protocol::RepositoryBindingId(binding_id),
                                VcsDiffQuery {
                                    side,
                                    path: path.clone(),
                                },
                            )
                            .await
                            .map_err(|error| format!("loading repository diff failed: {error}")),
                        Err(error) => Err(error),
                    };
                    let _ = events.send(ExecutorEvent::RepositoryDiffLoaded {
                        workspace_id,
                        request_id,
                        side,
                        path,
                        result,
                    });
                }));
            }
            ExecutorCommand::SetRepositoryHunkStaged {
                workspace_id,
                binding_id,
                expected_revision,
                request_id,
                side,
                path,
                hunk_id,
                line_indices,
                staged,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        let request = VcsHunkRequest {
                            expected_revision,
                            side,
                            path: path.clone(),
                            hunk_id,
                            line_indices,
                        };
                        let changed = if staged {
                            client.stage_repository_hunk(binding, request).await
                        } else {
                            client.unstage_repository_hunk(binding, request).await
                        };
                        match changed {
                            Ok(binding) => match client.repository_status(binding.id).await {
                                Ok(status) => client
                                    .repository_diff(
                                        binding.id,
                                        VcsDiffQuery {
                                            side,
                                            path: Some(path.clone()),
                                        },
                                    )
                                    .await
                                    .map(|diff| (status, diff))
                                    .map_err(|error| {
                                        format!("refreshing repository diff failed: {error}")
                                    }),
                                Err(error) => {
                                    Err(format!("refreshing repository status failed: {error}"))
                                }
                            },
                            Err(error) => Err(format!("updating repository hunk failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHunkUpdated {
                        workspace_id,
                        request_id,
                        side,
                        path,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DiscardRepositoryPath {
                workspace_id,
                binding_id,
                expected_revision,
                request_id,
                path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        match client
                            .discard_repository_path(
                                binding,
                                VcsDiscardRequest {
                                    expected_revision,
                                    path,
                                },
                            )
                            .await
                        {
                            Ok(mutation) => client
                                .repository_status(binding)
                                .await
                                .map(|status| (mutation, status))
                                .map_err(|error| {
                                    format!("refreshing repository status failed: {error}")
                                }),
                            Err(error) => {
                                Err(format!("discarding repository path failed: {error}"))
                            }
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryPathDiscarded {
                        workspace_id,
                        request_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RevertRepositoryHunk {
                workspace_id,
                binding_id,
                expected_revision,
                request_id,
                side,
                path,
                hunk_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let binding = sift_protocol::RepositoryBindingId(binding_id);
                        match client
                            .revert_repository_hunk(
                                binding,
                                VcsRevertHunkRequest {
                                    expected_revision,
                                    side,
                                    path: path.clone(),
                                    hunk_id,
                                },
                            )
                            .await
                        {
                            Ok(mutation) => match client.repository_status(binding).await {
                                Ok(status) => client
                                    .repository_diff(
                                        binding,
                                        VcsDiffQuery {
                                            side,
                                            path: Some(path.clone()),
                                        },
                                    )
                                    .await
                                    .map(|diff| (mutation, status, diff))
                                    .map_err(|error| {
                                        format!("refreshing repository diff failed: {error}")
                                    }),
                                Err(error) => {
                                    Err(format!("refreshing repository status failed: {error}"))
                                }
                            },
                            Err(error) => Err(format!("reverting repository hunk failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RepositoryHunkReverted {
                        workspace_id,
                        request_id,
                        side,
                        path,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadAutomations {
                workspace_id,
                git_commit,
            } => {
                let server = targets.borrow().clone();
                let (result, nodes) = match server.client().await {
                    Ok(client) => {
                        let workspace = sift_protocol::WorkspaceId(workspace_id);
                        let result = async {
                            let configurations = client.run_configurations(workspace).await?;
                            let last_success = match git_commit {
                                Some(commit) => {
                                    client
                                        .latest_successful_run_for_commit(workspace, &commit)
                                        .await?
                                }
                                None => None,
                            };
                            Ok::<_, sift_client_sdk::Error>((configurations, last_success))
                        }
                        .await
                        .map_err(|error| format!("loading automations failed: {error}"));
                        let nodes = client
                            .workspace_nodes(workspace)
                            .await
                            .map(|tree| tree.nodes)
                            .map_err(|error| format!("loading automation scripts failed: {error}"));
                        (result, nodes)
                    }
                    Err(error) => (Err(error.clone()), Err(error)),
                };
                if events
                    .send(ExecutorEvent::AutomationsLoaded {
                        workspace_id,
                        result,
                        nodes,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SaveRunConfiguration {
                item_id,
                workspace_id,
                configuration_id,
                expected_revision,
                request,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let saved = match configuration_id {
                            Some(configuration_id) => {
                                let Some(expected_revision) = expected_revision else {
                                    let _ = events.send(ExecutorEvent::RunConfigurationSaved {
                                        item_id,
                                        result: Err(
                                            "updating run configuration requires a revision".into(),
                                        ),
                                    });
                                    continue;
                                };
                                client
                                    .update_run_configuration(
                                        configuration_id,
                                        sift_api_types::UpdateRunConfigurationRequest {
                                            expected_revision,
                                            configuration: request,
                                        },
                                    )
                                    .await
                            }
                            None => {
                                client
                                    .create_run_configuration(
                                        sift_protocol::WorkspaceId(workspace_id),
                                        request,
                                    )
                                    .await
                            }
                        };
                        match saved {
                            Ok(configuration) => {
                                let validation = client
                                    .validate_run_configuration(configuration.id)
                                    .await
                                    .map_err(|error| {
                                        format!(
                                            "configuration saved but validation failed: {error}"
                                        )
                                    });
                                Ok((configuration, validation))
                            }
                            Err(error) => Err(format!("saving run configuration failed: {error}")),
                        }
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RunConfigurationSaved { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteRunConfiguration {
                item_id,
                configuration_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_run_configuration(
                            configuration_id,
                            sift_api_types::ExpectedRunConfigurationRevisionRequest {
                                expected_revision,
                            },
                        )
                        .await
                        .map_err(|error| format!("deleting run configuration failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::RunConfigurationDeleted {
                        item_id,
                        configuration_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::StartAutomation {
                configuration_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .start_run(
                            sift_protocol::RunConfigurationId(configuration_id),
                            StartRunRequest {
                                expected_configuration_revision: expected_revision,
                                variables: Default::default(),
                                timeout_secs: None,
                            },
                        )
                        .await
                        .map_err(|error| format!("starting automation failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationRunUpdated(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CancelAutomation { run_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .cancel_run(sift_protocol::RunId(run_id))
                        .await
                        .map_err(|error| format!("cancelling automation failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationRunUpdated(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadAutomationDetails {
                configuration_id,
                run_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => async {
                        let schedules = client.run_schedules(configuration_id).await?;
                        let mut occurrences = Vec::new();
                        for schedule in &schedules {
                            occurrences.extend(
                                client
                                    .schedule_occurrences(
                                        schedule.id,
                                        sift_api_types::ScheduleOccurrenceQuery { limit: 100 },
                                    )
                                    .await?,
                            );
                        }
                        occurrences
                            .sort_by_key(|occurrence| std::cmp::Reverse(occurrence.scheduled_for));
                        let detail_run_id = run_id.or_else(|| {
                            occurrences.iter().find_map(|occurrence| occurrence.run_id)
                        });
                        let (run, steps, logs) = match detail_run_id {
                            Some(run_id) => {
                                let run = client.run(run_id).await?;
                                let steps = client.run_steps(run_id).await?;
                                let logs = client
                                    .run_logs(
                                        run_id,
                                        sift_api_types::RunLogQuery {
                                            after: 0,
                                            limit: 200,
                                        },
                                    )
                                    .await?;
                                (Some(run), steps, logs)
                            }
                            None => (None, Vec::new(), Vec::new()),
                        };
                        Ok::<_, sift_client_sdk::Error>(
                            sift_workspace_ui::AutomationDetailsSnapshot {
                                schedules,
                                occurrences,
                                run,
                                steps,
                                logs,
                            },
                        )
                    }
                    .await
                    .map_err(|error| format!("loading automation details failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationDetailsLoaded {
                        configuration_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SaveAutomationSchedule {
                configuration_id,
                schedule_id,
                expected_revision,
                request,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => match schedule_id {
                        Some(schedule_id) => match expected_revision {
                            Some(expected_revision) => client
                                .update_run_schedule(
                                    schedule_id,
                                    sift_api_types::UpdateRunScheduleRequest {
                                        expected_revision,
                                        schedule: request,
                                    },
                                )
                                .await
                                .map(|_| ()),
                            None => Err(sift_client_sdk::Error::Protocol(
                                "updating a schedule requires a revision".into(),
                            )),
                        },
                        None => client
                            .create_run_schedule(configuration_id, request)
                            .await
                            .map(|_| ()),
                    }
                    .map_err(|error| format!("saving automation schedule failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationScheduleMutationFinished {
                        configuration_id,
                        action: "Automation schedule saved",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SetAutomationScheduleEnabled {
                configuration_id,
                schedule_id,
                expected_revision,
                enabled,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => {
                        let request = sift_api_types::ExpectedRunConfigurationRevisionRequest {
                            expected_revision,
                        };
                        if enabled {
                            client.enable_run_schedule(schedule_id, request).await
                        } else {
                            client.disable_run_schedule(schedule_id, request).await
                        }
                        .map(|_| ())
                        .map_err(|error| format!("updating automation schedule failed: {error}"))
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationScheduleMutationFinished {
                        configuration_id,
                        action: if enabled {
                            "Automation schedule enabled"
                        } else {
                            "Automation schedule disabled"
                        },
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteAutomationSchedule {
                configuration_id,
                schedule_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_run_schedule(
                            schedule_id,
                            sift_api_types::ExpectedRunConfigurationRevisionRequest {
                                expected_revision,
                            },
                        )
                        .await
                        .map_err(|error| format!("deleting automation schedule failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationScheduleMutationFinished {
                        configuration_id,
                        action: "Automation schedule deleted",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ResumeAutomationOccurrence {
                configuration_id,
                occurrence_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .resume_schedule_occurrence(occurrence_id)
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("resuming scheduled automation failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::AutomationScheduleMutationFinished {
                        configuration_id,
                        action: "Scheduled automation resumed",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadTransferRecipes { workspace_id } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .transfer_recipes(workspace_id)
                        .await
                        .map_err(|error| format!("loading transfer recipes failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::TransferRecipesLoaded {
                        workspace_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::SaveTransferRecipe {
                workspace_id,
                recipe_id,
                expected_revision,
                request,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => match recipe_id {
                        Some(recipe_id) => match expected_revision {
                            Some(expected_revision) => client
                                .update_transfer_recipe(
                                    recipe_id,
                                    sift_api_types::UpdateTransferRecipeRequest {
                                        expected_revision,
                                        recipe: request,
                                    },
                                )
                                .await
                                .map(|_| ()),
                            None => Err(sift_client_sdk::Error::Protocol(
                                "updating a transfer recipe requires a revision".into(),
                            )),
                        },
                        None => client
                            .create_transfer_recipe(workspace_id, request)
                            .await
                            .map(|_| ()),
                    }
                    .map_err(|error| format!("saving transfer recipe failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::TransferRecipeMutationFinished {
                        workspace_id,
                        action: "Transfer recipe saved",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteTransferRecipe {
                workspace_id,
                recipe_id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .delete_transfer_recipe(
                            recipe_id,
                            sift_api_types::ExpectedTransferRecipeRevisionRequest {
                                expected_revision,
                            },
                        )
                        .await
                        .map_err(|error| format!("deleting transfer recipe failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::TransferRecipeMutationFinished {
                        workspace_id,
                        action: "Transfer recipe deleted",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ValidateTransferRecipe {
                workspace_id,
                recipe_id,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .validate_transfer_recipe(recipe_id)
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("validating transfer recipe failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::TransferRecipeMutationFinished {
                        workspace_id,
                        action: "Transfer recipe validated",
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ExecuteTransferRecipe {
                generation,
                recipe_id,
                sql,
                data,
                table,
                sheet,
                create_table,
                conflict_policy,
            } => {
                let Some(opened) = context.as_ref() else {
                    let _ = events.send(ExecutorEvent::TransferRecipeExecutionFinished {
                        generation,
                        result: Err("Connect before executing a transfer recipe".into()),
                    });
                    continue;
                };
                active_transfers.clear();
                let (cancel, cancelled) = tokio::sync::oneshot::channel();
                active_transfers.insert(generation, cancel);
                let client = opened.client.clone();
                let session_id = opened.session;
                let connection_id = opened.connection;
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    let request = sift_api_types::ExecuteTransferRecipeRequest {
                        session_id,
                        connection_id,
                        sql,
                        params: Vec::new(),
                        data,
                        table,
                        sheet,
                        create_table,
                        conflict_policy: Some(conflict_policy),
                    };
                    let result = tokio::select! {
                        result = client.execute_transfer_recipe(recipe_id, request) => result
                            .map_err(|error| format!("executing transfer recipe failed: {error}")),
                        _ = cancelled => Err("Transfer request cancelled locally".into()),
                    };
                    let _ = events.send(ExecutorEvent::TransferRecipeExecutionFinished {
                        generation,
                        result,
                    });
                }));
            }
            ExecutorCommand::CancelTransferRecipe { generation } => {
                active_transfers.remove(&generation);
            }
            ExecutorCommand::LoadCatalogDiagram => {
                let result = match context.as_ref() {
                    Some(opened) => match opened
                        .client
                        .catalog_graph(
                            opened.session,
                            opened.metadata_connection,
                            sift_protocol::CatalogGraphRequest::default(),
                        )
                        .await
                    {
                        Ok(graph) => opened
                            .client
                            .catalog_diagram(
                                opened.session,
                                opened.metadata_connection,
                                sift_protocol::CatalogDiagramRequest {
                                    expected_revision: graph.revision,
                                    schemas: Vec::new(),
                                    object_ids: Vec::new(),
                                    edge_kinds: Vec::new(),
                                    neighborhood_depth: 1,
                                    include_columns: true,
                                    include_routines: false,
                                    max_nodes: Some(200),
                                },
                            )
                            .await
                            .map(Box::new)
                            .map_err(|error| format!("loading catalog diagram failed: {error}")),
                        Err(error) => Err(format!("loading catalog graph failed: {error}")),
                    },
                    None => Err("Connect before loading the catalog diagram".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogDiagramLoaded(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::TerminateDatabaseProcess { process_id } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .kill_process(opened.session, opened.metadata_connection, process_id)
                        .await
                        .map(|response| response.terminated)
                        .map_err(|error| format!("terminating database process failed: {error}")),
                    None => Err("Connect before terminating database activity".into()),
                };
                if events
                    .send(ExecutorEvent::DatabaseProcessTerminated { process_id, result })
                    .is_err()
                {
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
            ExecutorCommand::SearchSchema {
                generation,
                request,
            } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .search_schema(opened.session, opened.metadata_connection, request)
                        .await
                        .map(|response| ExecutorEvent::SchemaSearchLoaded {
                            generation,
                            response: Box::new(response),
                        })
                        .unwrap_or_else(|error| ExecutorEvent::SchemaSearchFailed {
                            generation,
                            message: format!("searching schema failed: {error}"),
                        }),
                    None => ExecutorEvent::SchemaSearchFailed {
                        generation,
                        message: "Connect to a database before searching its schema".into(),
                    },
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            ExecutorCommand::SearchData {
                generation,
                request,
            } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .search_data(opened.session, opened.metadata_connection, request)
                        .await
                        .map(|response| ExecutorEvent::DataSearchLoaded {
                            generation,
                            response: Box::new(response),
                        })
                        .unwrap_or_else(|error| ExecutorEvent::DataSearchFailed {
                            generation,
                            message: format!("searching table data failed: {error}"),
                        }),
                    None => ExecutorEvent::DataSearchFailed {
                        generation,
                        message: "Connect to a database before searching table data".into(),
                    },
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            ExecutorCommand::ImportCsv { request } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .import_csv(opened.session, opened.connection, request)
                        .await
                        .map_err(|error| format!("importing CSV failed: {error}")),
                    None => Err("Connect to a database before importing CSV data".into()),
                };
                if events.send(ExecutorEvent::CsvImported(result)).is_err() {
                    return;
                }
            }
            ExecutorCommand::CaptureCatalogSnapshot => {
                let result = match context.as_ref() {
                    Some(opened) => async {
                        let graph = opened
                            .client
                            .catalog_graph(
                                opened.session,
                                opened.metadata_connection,
                                sift_protocol::CatalogGraphRequest::default(),
                            )
                            .await?;
                        opened
                            .client
                            .create_catalog_snapshot(
                                opened.session,
                                opened.metadata_connection,
                                sift_protocol::CreateCatalogSnapshotRequest {
                                    expected_catalog_revision: graph.revision,
                                    options: Default::default(),
                                    description: Some("Desktop schema baseline".into()),
                                    accept_partial: false,
                                },
                            )
                            .await
                    }
                    .await
                    .map_err(|error| format!("capturing schema baseline failed: {error}")),
                    None => Err("Connect before capturing a schema baseline".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogSnapshotCaptured(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadCatalogSnapshots => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .catalog_snapshots(sift_api_types::TenantId(opened.tenant_id), 100)
                        .await
                        .map(|snapshots| {
                            snapshots
                                .into_iter()
                                .filter(|snapshot| {
                                    snapshot.connection_profile_id == Some(opened.profile_id)
                                })
                                .collect()
                        })
                        .map_err(|error| format!("loading schema baselines failed: {error}")),
                    None => Err("Connect before loading schema baselines".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogSnapshotsLoaded(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::PrepareCatalogMigration { baseline } => {
                let result = match context.as_ref() {
                    Some(opened) => async {
                        let live = opened
                            .client
                            .catalog_graph(
                                opened.session,
                                opened.metadata_connection,
                                sift_protocol::CatalogGraphRequest::default(),
                            )
                            .await?;
                        let snapshots = opened
                            .client
                            .catalog_snapshots(sift_api_types::TenantId(opened.tenant_id), 100)
                            .await?;
                        let baseline = snapshots
                            .into_iter()
                            .filter(|snapshot| {
                                snapshot.connection_profile_id == Some(opened.profile_id)
                            })
                            .find(|snapshot| baseline.is_none_or(|id| snapshot.id == id))
                            .ok_or_else(|| {
                                sift_client_sdk::Error::Protocol(
                                    "Capture a schema baseline for this connection first".into(),
                                )
                            })?;
                        let diff_request = sift_protocol::SchemaDiffRequest {
                            from: sift_protocol::CatalogSourceRef::Snapshot {
                                snapshot_id: baseline.id,
                            },
                            to: sift_protocol::CatalogSourceRef::Live {
                                expected_revision: live.revision,
                                options: Default::default(),
                            },
                            accepted_renames: Vec::new(),
                            max_changes: Some(2_000),
                        };
                        let diff = opened
                            .client
                            .compare_catalog_schemas(
                                opened.session,
                                opened.metadata_connection,
                                diff_request.clone(),
                            )
                            .await?;
                        let plan = opened
                            .client
                            .preview_migration(
                                opened.session,
                                opened.metadata_connection,
                                sift_protocol::PreviewMigrationRequest {
                                    diff: diff_request,
                                    expected_diff_digest: diff.digest.clone(),
                                    selected_changes: diff
                                        .changes
                                        .iter()
                                        .map(|change| change.id.clone())
                                        .collect(),
                                    expected_live_revision: live.revision,
                                    options: Default::default(),
                                },
                            )
                            .await?;
                        Ok::<_, sift_client_sdk::Error>((Box::new(diff), Box::new(plan)))
                    }
                    .await
                    .map_err(|error| format!("preparing schema migration failed: {error}")),
                    None => Err("Connect before comparing schemas".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogMigrationPrepared(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::PrepareWorkspaceDdlMigration { workspace_id } => {
                let result = match context.as_ref() {
                    Some(opened) => async {
                        let live = opened
                            .client
                            .catalog_graph(
                                opened.session,
                                opened.metadata_connection,
                                sift_protocol::CatalogGraphRequest::default(),
                            )
                            .await?;
                        let source = opened
                            .client
                            .ddl_sources(sift_protocol::WorkspaceId(workspace_id))
                            .await?
                            .into_iter()
                            .next()
                            .ok_or_else(|| sift_client_sdk::Error::Protocol(
                                "Create a workspace DDL source before comparing it to live schema".into(),
                            ))?;
                        let model = opened
                            .client
                            .refresh_ddl_source(
                                source.id,
                                sift_api_types::ExpectedDdlSourceRevisionRequest {
                                    expected_revision: source.revision,
                                },
                            )
                            .await?;
                        if model.diagnostics.iter().any(|diagnostic| diagnostic.error) {
                            return Err(sift_client_sdk::Error::Protocol(
                                "Workspace DDL source has parser errors".into(),
                            ));
                        }
                        let diff_request = sift_protocol::SchemaDiffRequest {
                            from: sift_protocol::CatalogSourceRef::Live {
                                expected_revision: live.revision,
                                options: Default::default(),
                            },
                            to: sift_protocol::CatalogSourceRef::DdlSource {
                                source_id: model.source.id,
                                expected_model_revision: model.source.model_revision,
                            },
                            accepted_renames: Vec::new(),
                            max_changes: Some(2_000),
                        };
                        let diff = opened.client.compare_catalog_schemas(
                            opened.session,
                            opened.metadata_connection,
                            diff_request.clone(),
                        ).await?;
                        let plan = opened.client.preview_migration(
                            opened.session,
                            opened.metadata_connection,
                            sift_protocol::PreviewMigrationRequest {
                                diff: diff_request,
                                expected_diff_digest: diff.digest.clone(),
                                selected_changes: diff.changes.iter().map(|change| change.id.clone()).collect(),
                                expected_live_revision: live.revision,
                                options: Default::default(),
                            },
                        ).await?;
                        Ok::<_, sift_client_sdk::Error>((Box::new(diff), Box::new(plan)))
                    }.await.map_err(|error| format!("comparing workspace DDL to live schema failed: {error}")),
                    None => Err("Connect before comparing workspace DDL".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogMigrationPrepared(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ApplyCatalogMigration { request } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .apply_migration(opened.session, opened.metadata_connection, request)
                        .await
                        .map(Box::new)
                        .map_err(|error| format!("applying schema migration failed: {error}")),
                    None => Err("Connect before applying a schema migration".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogMigrationApplied(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ValidateCatalogMigration { request } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .validate_migration(opened.session, opened.metadata_connection, request)
                        .await
                        .map(Box::new)
                        .map_err(|error| format!("validating schema migration failed: {error}")),
                    None => Err("Connect to the selected test database first".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogMigrationValidated(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::GenerateMigrationArtifacts {
                workspace_id,
                request,
                migration_path,
            } => {
                let server = targets.borrow().clone();
                let result = match server.client().await {
                    Ok(client) => client
                        .mutate_workspace_batch(sift_protocol::WorkspaceId(workspace_id), request)
                        .await
                        .map_err(|error| format!("generating migration artifacts failed: {error}")),
                    Err(error) => Err(error),
                };
                if events
                    .send(ExecutorEvent::MigrationArtifactsGenerated {
                        migration_path,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::RefreshCatalogMigrationRun { run } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .migration_run(opened.session, opened.metadata_connection, run)
                        .await
                        .map(Box::new)
                        .map_err(|error| format!("loading migration run failed: {error}")),
                    None => Err("Connect before loading a migration run".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogMigrationRunLoaded(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CancelCatalogMigrationRun { run } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .cancel_migration(opened.session, opened.metadata_connection, run)
                        .await
                        .map(|()| run)
                        .map_err(|error| format!("canceling migration failed: {error}")),
                    None => Err("Connect before canceling a migration".into()),
                };
                if events
                    .send(ExecutorEvent::CatalogMigrationCanceled(result))
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadHistory { item_id, cursor } => {
                let append = cursor.is_some();
                let connected_client = context.as_ref().map(|opened| opened.client.clone());
                let server = targets.borrow().clone();
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    let load = async {
                        let client = match connected_client {
                            Some(client) => client,
                            None => server.client().await?,
                        };
                        client
                            .history_page(None, cursor.as_deref(), Some(50))
                            .await
                            .map_err(|error| format!("loading query history failed: {error}"))
                    };
                    let page = tokio::time::timeout(HISTORY_LOAD_TIMEOUT, load)
                        .await
                        .unwrap_or_else(|_| {
                            Err("Loading query history timed out after 10 seconds".into())
                        });
                    let _ = events.send(ExecutorEvent::HistoryLoaded {
                        item_id,
                        append,
                        page,
                    });
                }));
            }
            ExecutorCommand::LoadGlobalHistory {
                instance_id,
                generation,
                cursor,
            } => {
                let append = cursor.is_some();
                let connected_client = context.as_ref().map(|opened| opened.client.clone());
                let server = targets.borrow().clone();
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    let load = async {
                        let client = match connected_client {
                            Some(client) => client,
                            None => server.client().await?,
                        };
                        client
                            .history_page(None, cursor.as_deref(), Some(100))
                            .await
                            .map_err(|error| format!("loading query history failed: {error}"))
                    };
                    let page = tokio::time::timeout(HISTORY_LOAD_TIMEOUT, load)
                        .await
                        .unwrap_or_else(|_| {
                            Err("Loading query history timed out after 10 seconds".into())
                        });
                    let _ = events.send(ExecutorEvent::GlobalHistoryLoaded {
                        instance_id,
                        generation,
                        append,
                        page,
                    });
                }));
            }
            ExecutorCommand::LoadSavedQueries { tenant_id } => {
                let server = targets.borrow().clone();
                let result = async {
                    let client = server.client().await?;
                    client
                        .saved_queries(
                            TenantId(tenant_id),
                            None,
                            &[],
                            Some(sift_api_types::SavedQueryScope::All),
                        )
                        .await
                        .map_err(|error| format!("loading saved queries failed: {error}"))
                }
                .await;
                if events
                    .send(ExecutorEvent::SavedQueriesLoaded { tenant_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadSavedQuery { item_id, id } => {
                let server = targets.borrow().clone();
                let result = async {
                    server
                        .client()
                        .await?
                        .saved_query(id)
                        .await
                        .map_err(|error| format!("loading saved query failed: {error}"))
                }
                .await;
                if events
                    .send(ExecutorEvent::SavedQueryLoaded { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CreateSavedQuery { item_id, request } => {
                let server = targets.borrow().clone();
                let result = async {
                    server
                        .client()
                        .await?
                        .create_saved_query(request)
                        .await
                        .map_err(|error| format!("saving query failed: {error}"))
                }
                .await;
                if events
                    .send(ExecutorEvent::SavedQuerySaved { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::UpdateSavedQuery {
                item_id,
                id,
                request,
            } => {
                let server = targets.borrow().clone();
                let result = async {
                    server
                        .client()
                        .await?
                        .update_saved_query(id, request)
                        .await
                        .map_err(|error| format!("updating saved query failed: {error}"))
                }
                .await;
                if events
                    .send(ExecutorEvent::SavedQuerySaved { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeleteSavedQuery {
                id,
                expected_revision,
            } => {
                let server = targets.borrow().clone();
                let result = async {
                    server
                        .client()
                        .await?
                        .delete_saved_query(id, expected_revision)
                        .await
                        .map_err(|error| format!("deleting saved query failed: {error}"))
                }
                .await;
                if events
                    .send(ExecutorEvent::SavedQueryDeleted { id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::PreviewResultEdits { item_id, edit_set } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .preview_edits(
                            opened.session,
                            opened.connection,
                            sift_protocol::PreviewEditsRequest {
                                connection: opened.connection,
                                edit_set: edit_set.clone(),
                            },
                        )
                        .await
                        .map_err(|error| format!("previewing result edit failed: {error}")),
                    None => Err("Connect to this table before previewing an edit".into()),
                };
                if events
                    .send(ExecutorEvent::ResultEditsPreviewed {
                        item_id,
                        edit_set,
                        result: event,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ApplyResultEdits { item_id, edit_set } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .apply_edits(
                            opened.session,
                            opened.connection,
                            sift_protocol::ApplyEditsRequest {
                                connection: opened.connection,
                                edit_set,
                                tx: opened.transaction.as_ref().map(|transaction| {
                                    sift_protocol::TxHandleRef {
                                        tx_id: transaction.tx_id,
                                        connection: transaction.connection,
                                        mode: transaction.mode,
                                    }
                                }),
                            },
                        )
                        .await
                        .map_err(|error| {
                            let conflict = match &error {
                                sift_client_sdk::Error::Server { error, .. } => {
                                    error.edit_conflict.clone()
                                }
                                _ => None,
                            };
                            sift_workspace_ui::ResultEditApplyFailure {
                                message: format!("applying result edit failed: {error}"),
                                conflict,
                            }
                        }),
                    None => Err(sift_workspace_ui::ResultEditApplyFailure {
                        message: "Connect to this table before applying an edit".into(),
                        conflict: None,
                    }),
                };
                if events
                    .send(ExecutorEvent::ResultEditsApplied {
                        item_id,
                        result: event,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ExportResult {
                item_id,
                destination,
                request,
            } => {
                let Some(opened) = context.as_ref() else {
                    let _ = events.send(ExecutorEvent::ResultExported {
                        item_id,
                        destination,
                        result: Err("Connect before exporting results".into()),
                    });
                    continue;
                };
                active_exports.remove(&item_id);
                let (cancel, cancelled) = tokio::sync::oneshot::channel();
                active_exports.insert(item_id, cancel);
                let client = opened.client.clone();
                let session = opened.session;
                let connection = opened.connection;
                let events = events.clone();
                std::mem::drop(tokio::spawn(async move {
                    let result = stream_result_export(
                        ExportRun {
                            client,
                            session,
                            connection,
                            request,
                            item_id,
                        },
                        &destination,
                        cancelled,
                        &events,
                    )
                    .await;
                    let _ = events.send(ExecutorEvent::ResultExported {
                        item_id,
                        destination,
                        result,
                    });
                }));
            }
            ExecutorCommand::CancelResultExport { item_id } => {
                active_exports.remove(&item_id);
            }
            ExecutorCommand::CapturePlan {
                item_id,
                sql,
                params,
                source,
            } => {
                let result = match context.as_ref() {
                    Some(opened) => capture_plan(opened, sql, params, source).await,
                    None => Err("Connect before saving a plan capture".into()),
                };
                if events
                    .send(ExecutorEvent::PlanCaptured { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadPlanCaptures {
                item_id,
                tenant_id,
                sql,
            } => {
                let result = match context.as_ref() {
                    Some(opened) => load_plan_captures(opened, tenant_id, sql).await,
                    None => Err("Connect before loading plan captures".into()),
                };
                if events
                    .send(ExecutorEvent::PlanCapturesLoaded { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::ComparePlanCaptures {
                item_id,
                tenant_id,
                left,
                right,
            } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .compare_plan_captures(
                            TenantId(tenant_id),
                            sift_protocol::ComparePlanCapturesRequest { left, right },
                        )
                        .await
                        .map_err(|error| format!("comparing plan captures failed: {error}")),
                    None => Err("Connect before comparing plan captures".into()),
                };
                if events
                    .send(ExecutorEvent::PlanCapturesCompared { item_id, result })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::DeletePlanCapture {
                item_id,
                tenant_id,
                capture_id,
                expected_revision,
            } => {
                let result = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .delete_plan_capture(TenantId(tenant_id), capture_id, expected_revision)
                        .await
                        .map_err(|error| format!("deleting plan capture failed: {error}")),
                    None => Err("Connect before deleting a plan capture".into()),
                };
                if events
                    .send(ExecutorEvent::PlanCaptureDeleted {
                        item_id,
                        capture_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::LoadObjectDdl { item_id, source } => {
                let event = match context.as_ref() {
                    Some(opened) if opened.profile_id == source.profile_id => {
                        let path = sift_protocol::ObjectPath {
                            catalog: source.catalog.clone(),
                            schema: Some(source.schema.clone()),
                            name: source.object.clone(),
                            kind: Some(source.object_kind),
                            routine_args: None,
                        };
                        opened
                            .client
                            .object_ddl(opened.session, opened.metadata_connection, &path)
                            .await
                            .map(|object| ExecutorEvent::ObjectDdlLoaded {
                                item_id,
                                ddl: object.ddl,
                            })
                            .unwrap_or_else(|error| ExecutorEvent::ObjectDdlFailed {
                                item_id,
                                message: format!("loading object DDL failed: {error}"),
                            })
                    }
                    _ => ExecutorEvent::ObjectDdlFailed {
                        item_id,
                        message: "Connect to this object before loading its DDL".into(),
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
                            opened.metadata_connection,
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
            ExecutorCommand::Semantic {
                item_id,
                text_revision,
                text,
                request,
            } => {
                let outline = matches!(request, SemanticRequestKind::Outline { .. });
                let job = SemanticJob {
                    item_id,
                    text_revision,
                    text,
                    request,
                };
                let delivered = context
                    .as_ref()
                    .is_some_and(|opened| opened.semantic.send(SemanticControl::Run(job)).is_ok());
                if !delivered
                    && events
                        .send(ExecutorEvent::Semantic {
                            item_id,
                            text_revision,
                            outcome: Box::new(if outline {
                                SemanticOutcome::OutlineFailed(
                                    "Not connected — query outline needs a connection.".into(),
                                )
                            } else {
                                SemanticOutcome::Failed(
                                    "Not connected — SQL analysis needs a connection.".into(),
                                )
                            }),
                        })
                        .is_err()
                {
                    return;
                }
            }
            ExecutorCommand::CloseSemanticDocument { item_id } => {
                if let Some(opened) = context.as_ref() {
                    let _ = opened.semantic.send(SemanticControl::Close(item_id));
                }
            }
            ExecutorCommand::ApplyTableMigration { item_id, request } => {
                let event = match context.as_ref() {
                    Some(opened) => opened
                        .client
                        .apply_migration(opened.session, opened.metadata_connection, request)
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

struct ExportRun {
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    request: sift_protocol::ExportRequest,
    item_id: u64,
}

async fn stream_result_export(
    run: ExportRun,
    destination: &std::path::Path,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Result<(), String> {
    let mut output = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await
        .map_err(|error| {
            format!("creating export (existing files are not overwritten): {error}")
        })?;
    let mut stream = match run
        .client
        .stream_export_query(run.session, run.connection, run.request)
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            let _ = tokio::fs::remove_file(destination).await;
            return Err(format!("exporting query failed: {error}"));
        }
    };
    let mut bytes = 0_u64;
    loop {
        let chunk = tokio::select! {
            _ = &mut cancelled => {
                let _ = tokio::fs::remove_file(destination).await;
                return Err("Export cancelled".into());
            }
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else { break };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = tokio::fs::remove_file(destination).await;
                return Err(format!("streaming export failed: {error}"));
            }
        };
        if let Err(error) = output.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(destination).await;
            return Err(format!("writing export failed: {error}"));
        }
        bytes = bytes.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        let _ = events.send(ExecutorEvent::ResultExportProgress {
            item_id: run.item_id,
            bytes,
        });
    }
    if let Err(error) = output.flush().await {
        let _ = tokio::fs::remove_file(destination).await;
        return Err(format!("finishing export failed: {error}"));
    }
    Ok(())
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
        .schema(context.session, context.metadata_connection)
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

async fn capture_plan(
    context: &QueryContext,
    sql: String,
    params: Vec<sift_protocol::Value>,
    source: Option<sift_protocol::VersionedExecutionContext>,
) -> Result<sift_protocol::PlanCapture, String> {
    let state = context
        .client
        .open_semantic_document(
            context.session,
            context.plan_connection,
            sift_protocol::CreateSemanticDocumentRequest {
                text: sql,
                source: None,
            },
        )
        .await
        .map_err(|error| format!("opening plan source failed: {error}"))?;
    let result = async {
        let graph = context
            .client
            .catalog_graph(
                context.session,
                context.plan_connection,
                sift_protocol::CatalogGraphRequest::default(),
            )
            .await
            .map_err(|error| format!("loading catalog revision failed: {error}"))?;
        let selection = context
            .client
            .select_semantic_statement(
                context.session,
                context.plan_connection,
                state.document_id,
                sift_protocol::SelectStatementRequest {
                    revision: state.revision,
                    cursor: 0,
                    selection: None,
                },
            )
            .await
            .map_err(|error| format!("selecting plan statement failed: {error}"))?;
        let statement = selection
            .statements
            .first()
            .ok_or_else(|| "No statement is available to capture".to_owned())?;
        context
            .client
            .capture_semantic_plan(
                context.session,
                context.plan_connection,
                sift_protocol::CaptureSemanticPlanRequest {
                    document_id: state.document_id,
                    revision: state.revision,
                    statement_id: statement.statement_id.clone(),
                    catalog_revision: graph.revision,
                    analyze: false,
                    params,
                    include_raw_response: false,
                    source,
                },
            )
            .await
            .map_err(|error| format!("saving plan capture failed: {error}"))
    }
    .await;
    let _ = context
        .client
        .close_semantic_document(context.session, context.plan_connection, state.document_id)
        .await;
    result
}

async fn load_plan_captures(
    context: &QueryContext,
    tenant_id: i64,
    sql: String,
) -> Result<Vec<sift_protocol::PlanCaptureSummary>, String> {
    let state = context
        .client
        .open_semantic_document(
            context.session,
            context.plan_connection,
            sift_protocol::CreateSemanticDocumentRequest {
                text: sql.clone(),
                source: None,
            },
        )
        .await
        .map_err(|error| format!("opening plan capture filter failed: {error}"))?;
    let fingerprint = sift_server::fingerprint::sql(&sql);
    let result = async {
        let mut captures = Vec::new();
        let mut cursor = None;
        loop {
            let page = context
                .client
                .plan_captures(
                    TenantId(tenant_id),
                    sift_protocol::ListPlanCapturesRequest {
                        source_digest: Some(state.source_digest.clone()),
                        cursor,
                        limit: Some(100),
                    },
                )
                .await
                .map_err(|error| format!("loading plan captures failed: {error}"))?;
            captures.extend(
                page.items
                    .into_iter()
                    .filter(|capture| capture.statement_fingerprint == fingerprint),
            );
            let Some(next) = page.next_cursor else {
                break;
            };
            let next = next
                .parse::<uuid::Uuid>()
                .map(sift_protocol::PlanCaptureId)
                .map_err(|_| "loading plan captures returned an invalid cursor".to_owned())?;
            if cursor == Some(next) {
                return Err("loading plan captures returned a repeated cursor".into());
            }
            cursor = Some(next);
        }
        Ok(captures)
    }
    .await;
    let _ = context
        .client
        .close_semantic_document(context.session, context.plan_connection, state.document_id)
        .await;
    result
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
        .catalog_graph(context.session, context.metadata_connection, request)
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
    request: UpsertConnectionProfileRequest,
) -> Result<sift_workspace_ui::ConnectionNavEntry, String> {
    let client = server.client().await?;
    let tenant_id = request.tenant_id;
    let name = request.name.clone();
    let profile = client
        .upsert_connection_profile(request)
        .await
        .map_err(|error| format!("saving connection profile failed: {error}"))?;
    Ok(sift_workspace_ui::ConnectionNavEntry {
        id: profile.id.0,
        tenant_id,
        name,
        provider_id: profile.provider_id,
        tags: profile.tags,
    })
}

async fn test_connection_profile(
    server: &DesktopServer,
    tenant_id: i64,
    provider_id: sift_protocol::ProviderId,
    configuration: serde_json::Value,
    credentials: Option<serde_json::Value>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Result<(), String> {
    let client = server.client().await?;
    let name = format!("__sift_connection_test_{}", uuid::Uuid::new_v4());
    let profile = client
        .upsert_connection_profile(UpsertConnectionProfileRequest {
            tenant_id,
            vault_id: None,
            name,
            provider_id,
            configuration,
            credentials,
            credential_mode: CredentialMode::Shared,
            tags: vec!["sift:temporary-connection-test".into()],
        })
        .await
        .map_err(|error| format!("preparing connection test failed: {error}"))?;
    let opened = open_query_context(server, tenant_id, profile.id.0, events).await;
    if let Ok(context) = &opened {
        let _ = context.client.close_session(context.session).await;
    }
    let cleanup = client
        .delete_connection_profile(TenantId(tenant_id), profile.id)
        .await
        .map_err(|error| format!("removing temporary test profile failed: {error}"));
    match (opened, cleanup) {
        (Ok(_), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

const SEMANTIC_COMPLETION_LIMIT: u32 = 50;
const SEMANTIC_USAGE_LIMIT: u32 = 500;

struct QueryRun {
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    transaction: Option<sift_protocol::TransactionInfo>,
    item_id: u64,
    execution_id: u64,
    sql: String,
    params: Vec<sift_protocol::Value>,
    transform: Option<sift_protocol::ResultTransform>,
    source: Option<sift_protocol::VersionedExecutionContext>,
    variable_context: Option<sift_protocol::SqlVariableHistoryContext>,
}

async fn run_streamed_query(
    run: QueryRun,
    controls: tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let client = run.client.clone();
    let session = run.session;
    let transaction = run.transaction.clone();
    run_streamed_query_inner(run, controls, events.clone()).await;
    let Some(transaction) = transaction else {
        return;
    };
    let state = client
        .list_transactions(session)
        .await
        .map(|states| {
            states
                .into_iter()
                .find(|state| state.transaction.tx_id == transaction.tx_id)
        })
        .map_err(|error| format!("refreshing transaction state failed: {error}"));
    let _ = events.send(ExecutorEvent::TransactionStateRefreshed(state));
}

async fn run_streamed_query_inner(
    run: QueryRun,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let QueryRun {
        client,
        session,
        connection,
        transaction,
        item_id,
        execution_id,
        sql,
        params,
        transform,
        source,
        variable_context,
    } = run;
    let started = tokio::select! {
        stream = client.start_query_event_stream_versioned_with_variables(
            session,
            connection,
            sql,
            params,
            transaction.map(|transaction| sift_protocol::TxHandleRef {
                tx_id: transaction.tx_id,
                connection: transaction.connection,
                mode: transaction.mode,
            }),
            transform,
            source,
            variable_context,
        ) => stream,
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
            page = stream.next_events() => page,
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
        let (seq, execution_events) = match next {
            Ok(page) => page,
            Err(error) => {
                send_execution_error(item_id, execution_id, error, &events);
                return;
            }
        };
        let mut terminal = false;
        let mut batch_warnings = Vec::new();
        for event in execution_events {
            let page = match event {
                sift_protocol::ExecutionEventV2::ResultSetStarted { columns, .. } => {
                    Some(sift_protocol::Page::NextResult { columns })
                }
                sift_protocol::ExecutionEventV2::Rows { rows, .. } => {
                    Some(sift_protocol::Page::Rows { rows })
                }
                sift_protocol::ExecutionEventV2::ResultSetCompleted { summary, .. } => {
                    batch_warnings.extend(summary.warnings);
                    None
                }
                sift_protocol::ExecutionEventV2::CommandCompleted { summary, .. } => {
                    batch_warnings.extend(summary.warnings);
                    None
                }
                sift_protocol::ExecutionEventV2::Notice { message, .. } => {
                    batch_warnings.push(sift_protocol::DriverWarning::new(message));
                    None
                }
                sift_protocol::ExecutionEventV2::ExecutionCompleted { summary, .. } => {
                    terminal = true;
                    Some(sift_protocol::Page::Done {
                        affected_rows: summary.affected_rows,
                        warnings: std::mem::take(&mut batch_warnings),
                    })
                }
                sift_protocol::ExecutionEventV2::Error { error, .. } => {
                    if error.code == sift_protocol::Code::CursorEvicted
                        && error.resume_url.is_some()
                    {
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
                    terminal = true;
                    Some(sift_protocol::Page::Error { error })
                }
                sift_protocol::ExecutionEventV2::Progress { progress, .. } => {
                    let _ = events.send(ExecutorEvent::ExecutionProgress {
                        item_id,
                        execution_id,
                        progress,
                    });
                    None
                }
                sift_protocol::ExecutionEventV2::ExecutionStarted { .. }
                | sift_protocol::ExecutionEventV2::StatementStarted { .. } => None,
            };
            let Some(page) = page else { continue };
            let (acknowledge, consumed) = tokio::sync::oneshot::channel();
            if events
                .send(ExecutorEvent::ExecutionPage {
                    item_id,
                    execution_id,
                    cursor_id,
                    page: sift_workspace_ui::results::PreparedResultPage::new(page),
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
            let consumed = tokio::select! {
                consumed = consumed => consumed.is_ok(),
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
            if !consumed {
                let _ = stream.cancel().await;
                return;
            }
        }
        if terminal {
            return;
        }
        tokio::select! {
            acknowledged = stream.acknowledge(seq) => {
                if acknowledged.is_err() {
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
                    page: sift_workspace_ui::results::PreparedResultPage::new(page),
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
                    page: sift_workspace_ui::results::PreparedResultPage::new(
                        sift_protocol::Page::Done {
                            affected_rows: None,
                            warnings: vec![sift_protocol::DriverWarning::new(
                                "Cursor resumed after eviction; only retained spill pages are available.",
                            )],
                        },
                    ),
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
/// One unit of semantic work, addressed to the exact buffer revision it
/// describes. The text travels with the job so the server document can be
/// resynchronized without relying on the order two channels happened to be
/// drained in.
struct SemanticJob {
    item_id: u64,
    text_revision: u64,
    text: String,
    request: SemanticRequestKind,
}

enum SemanticControl {
    Run(SemanticJob),
    Close(u64),
}

/// Server-side identity of one editor's semantic document.
struct SemanticDocument {
    id: sift_protocol::SemanticDocumentId,
    /// Server document revision, required verbatim by every read operation.
    revision: u64,
    /// Client text revision the server text currently matches.
    text_revision: u64,
}

/// Owns every server semantic document for one connection.
///
/// Runs on its own task so a slow analysis never delays query execution, and
/// processes jobs sequentially because the server document is stateful: two
/// concurrent updates would race on `base_revision`. Each pass drains
/// everything already queued first, which is where revision cancellation
/// happens — superseded work is discarded before it costs a round trip.
async fn run_semantic_service(
    client: Client,
    session: SessionId,
    connection: ConnectionId,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<SemanticControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let mut documents: HashMap<u64, SemanticDocument> = HashMap::new();
    let mut catalog_revision: Option<sift_protocol::CatalogRevision> = None;
    while let Some(first) = controls.recv().await {
        let mut batch = vec![first];
        while let Ok(next) = controls.try_recv() {
            batch.push(next);
        }
        let closed = batch
            .iter()
            .filter_map(|control| match control {
                SemanticControl::Close(item_id) => Some(*item_id),
                SemanticControl::Run(_) => None,
            })
            .collect::<HashSet<_>>();
        for item_id in &closed {
            if let Some(document) = documents.remove(item_id) {
                let _ = client
                    .close_semantic_document(session, connection, document.id)
                    .await;
            }
        }
        let jobs = admissible_jobs(batch, &closed, &events);
        for job in jobs {
            if run_semantic_job(
                &client,
                session,
                connection,
                &mut documents,
                &mut catalog_revision,
                job,
                &events,
            )
            .await
            .is_err()
            {
                return;
            }
        }
    }
    for (_, document) in documents.drain() {
        let _ = client
            .close_semantic_document(session, connection, document.id)
            .await;
    }
}

/// Reduce one drained batch to the work still worth doing: newest revision per
/// item, and at most one `Analyze` for it. A superseded interactive request is
/// reported back rather than dropped silently, so the editor never waits on an
/// answer that will never arrive.
fn admissible_jobs(
    batch: Vec<SemanticControl>,
    closed: &HashSet<u64>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Vec<SemanticJob> {
    let mut jobs = batch
        .into_iter()
        .filter_map(|control| match control {
            SemanticControl::Run(job) if !closed.contains(&job.item_id) => Some(job),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut newest: HashMap<u64, u64> = HashMap::new();
    for job in &jobs {
        let entry = newest.entry(job.item_id).or_insert(job.text_revision);
        *entry = (*entry).max(job.text_revision);
    }
    // Walking backwards keeps the last Analyze of a burst, which is the one
    // whose answer matches what the user is now looking at.
    let mut analyzed: HashSet<u64> = HashSet::new();
    let mut hovered: HashSet<u64> = HashSet::new();
    let mut kept = Vec::with_capacity(jobs.len());
    for job in jobs.drain(..).rev() {
        let current = newest.get(&job.item_id).copied() == Some(job.text_revision);
        let duplicate_analyze =
            job.request == SemanticRequestKind::Analyze && !analyzed.insert(job.item_id);
        let duplicate_hover = matches!(job.request, SemanticRequestKind::Hover { .. })
            && !hovered.insert(job.item_id);
        if current && !duplicate_analyze && !duplicate_hover {
            kept.push(job);
            continue;
        }
        if job.request != SemanticRequestKind::Analyze && !duplicate_hover {
            let outcome = if matches!(job.request, SemanticRequestKind::Outline { .. }) {
                SemanticOutcome::OutlineFailed("Buffer changed before the request ran.".into())
            } else {
                SemanticOutcome::Failed("Buffer changed before the request ran.".into())
            };
            let _ = events.send(ExecutorEvent::Semantic {
                item_id: job.item_id,
                text_revision: job.text_revision,
                outcome: Box::new(outcome),
            });
        }
    }
    kept.reverse();
    kept
}

/// Bring the server document in line with `job.text`, returning the server
/// revision to quote in the request. Opening a fresh document is the recovery
/// path for any update failure: an out-of-date `base_revision` is not
/// something the client can reconcile, and the text is authoritative here.
async fn sync_semantic_document(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    documents: &mut HashMap<u64, SemanticDocument>,
    job: &SemanticJob,
) -> Result<(sift_protocol::SemanticDocumentId, u64), String> {
    if let Some(existing) = documents.get(&job.item_id) {
        if existing.text_revision == job.text_revision {
            return Ok((existing.id, existing.revision));
        }
        let updated = client
            .update_semantic_document(
                session,
                connection,
                existing.id,
                sift_protocol::UpdateSemanticDocumentRequest {
                    base_revision: existing.revision,
                    text: job.text.clone(),
                },
            )
            .await;
        match updated {
            Ok(state) => {
                documents.insert(
                    job.item_id,
                    SemanticDocument {
                        id: state.document_id,
                        revision: state.revision,
                        text_revision: job.text_revision,
                    },
                );
                return Ok((state.document_id, state.revision));
            }
            Err(_) => {
                documents.remove(&job.item_id);
            }
        }
    }
    let state = client
        .open_semantic_document(
            session,
            connection,
            sift_protocol::CreateSemanticDocumentRequest {
                text: job.text.clone(),
                source: Some(sift_protocol::SemanticSource::Scratch),
            },
        )
        .await
        .map_err(|error| format!("SQL analysis is unavailable: {error}"))?;
    documents.insert(
        job.item_id,
        SemanticDocument {
            id: state.document_id,
            revision: state.revision,
            text_revision: job.text_revision,
        },
    );
    Ok((state.document_id, state.revision))
}

/// The catalog revision the semantic endpoints will accept. Both the catalog
/// diagnostics and quick-fix routes rebuild the graph from the *default*
/// request and reject anything but its current revision, so this must ask for
/// exactly that shape.
async fn current_catalog_revision(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    cached: &mut Option<sift_protocol::CatalogRevision>,
) -> Option<sift_protocol::CatalogRevision> {
    if let Some(revision) = *cached {
        return Some(revision);
    }
    let graph = client
        .catalog_graph(
            session,
            connection,
            sift_protocol::CatalogGraphRequest::default(),
        )
        .await
        .ok()?;
    *cached = Some(graph.revision);
    *cached
}

/// Run one job. `Err(())` means the UI channel closed and the service should
/// stop; every other failure is reported to the editor as an outcome.
#[allow(clippy::result_unit_err)]
async fn run_semantic_job(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    documents: &mut HashMap<u64, SemanticDocument>,
    catalog_revision: &mut Option<sift_protocol::CatalogRevision>,
    job: SemanticJob,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Result<(), ()> {
    let item_id = job.item_id;
    let text_revision = job.text_revision;
    let outcome = match sync_semantic_document(client, session, connection, documents, &job).await {
        Ok((document, revision)) => {
            semantic_outcome(
                client,
                session,
                connection,
                document,
                revision,
                catalog_revision,
                job.request,
            )
            .await
        }
        Err(message) => SemanticOutcome::Failed(message),
    };
    events
        .send(ExecutorEvent::Semantic {
            item_id,
            text_revision,
            outcome: Box::new(outcome),
        })
        .map_err(|_| ())
}

async fn semantic_outcome(
    client: &Client,
    session: SessionId,
    connection: ConnectionId,
    document: sift_protocol::SemanticDocumentId,
    revision: u64,
    catalog_revision: &mut Option<sift_protocol::CatalogRevision>,
    request: SemanticRequestKind,
) -> SemanticOutcome {
    match request {
        SemanticRequestKind::Analyze => {
            // Catalog-bound diagnostics are strictly better but need a live
            // catalog revision. When it is stale or unavailable, fall back to
            // syntax-only diagnostics instead of showing the user nothing.
            if let Some(catalog) =
                current_catalog_revision(client, session, connection, catalog_revision).await
            {
                match client
                    .semantic_diagnostics_with_catalog(
                        session, connection, document, revision, catalog,
                    )
                    .await
                {
                    Ok(response) => {
                        return SemanticOutcome::Diagnostics {
                            diagnostics: response.diagnostics,
                            incomplete: response.incomplete,
                        }
                    }
                    Err(_) => *catalog_revision = None,
                }
            }
            match client
                .semantic_diagnostics(session, connection, document, revision)
                .await
            {
                Ok(response) => SemanticOutcome::Diagnostics {
                    diagnostics: response.diagnostics,
                    incomplete: true,
                },
                Err(error) => SemanticOutcome::Failed(format!("diagnostics failed: {error}")),
            }
        }
        SemanticRequestKind::Complete { cursor } => {
            match client
                .complete_semantic_document(
                    session,
                    connection,
                    document,
                    sift_protocol::SemanticCompletionRequest {
                        revision,
                        cursor,
                        limit: Some(SEMANTIC_COMPLETION_LIMIT),
                    },
                )
                .await
            {
                Ok(response) => SemanticOutcome::Completions {
                    replaced: sift_protocol::TextRange {
                        start: response.replaced_range.start,
                        end: response.replaced_range.end,
                    },
                    candidates: response.candidates,
                },
                Err(error) => SemanticOutcome::Failed(format!("completion failed: {error}")),
            }
        }
        SemanticRequestKind::Hover { position } => {
            let catalog =
                current_catalog_revision(client, session, connection, catalog_revision).await;
            match client
                .hover_semantic_document(
                    session,
                    connection,
                    document,
                    sift_protocol::SemanticHoverRequest {
                        revision,
                        position,
                        catalog_revision: catalog,
                    },
                )
                .await
            {
                Ok(response) => SemanticOutcome::Hover(response),
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("hover failed: {error}"))
                }
            }
        }
        SemanticRequestKind::ExpandStar { position } => {
            let Some(catalog) =
                current_catalog_revision(client, session, connection, catalog_revision).await
            else {
                return SemanticOutcome::Failed(
                    "Star expansion needs complete catalog metadata.".into(),
                );
            };
            match client
                .prepare_star_expansion(
                    session,
                    connection,
                    document,
                    sift_protocol::PrepareStarExpansionRequest {
                        revision,
                        position,
                        catalog_revision: catalog,
                    },
                )
                .await
            {
                Ok(preview) => SemanticOutcome::StarExpansion(preview),
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("star expansion failed: {error}"))
                }
            }
        }
        SemanticRequestKind::Format { range } => {
            match client
                .format_semantic_document(
                    session,
                    connection,
                    document,
                    sift_protocol::FormatSqlRequest {
                        revision,
                        range,
                        options: sift_protocol::FormatOptions::default(),
                    },
                )
                .await
            {
                Ok(edit) => workspace_edit_outcome(edit, document),
                Err(error) => SemanticOutcome::Failed(format!("formatting failed: {error}")),
            }
        }
        SemanticRequestKind::QuickFix { fix_id } => {
            let Some(catalog) =
                current_catalog_revision(client, session, connection, catalog_revision).await
            else {
                return SemanticOutcome::Failed(
                    "Quick fixes need catalog metadata this connection cannot provide.".into(),
                );
            };
            match client
                .prepare_semantic_quick_fix(
                    session,
                    connection,
                    document,
                    &fix_id,
                    sift_protocol::SqlQuickFixRequest {
                        revision,
                        catalog_revision: catalog,
                    },
                )
                .await
            {
                Ok(edit) => workspace_edit_outcome(edit, document),
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("quick fix failed: {error}"))
                }
            }
        }
        SemanticRequestKind::Usages { position } => {
            let catalog =
                current_catalog_revision(client, session, connection, catalog_revision).await;
            match client
                .find_semantic_usages(
                    session,
                    connection,
                    document,
                    sift_protocol::FindSqlUsagesRequest {
                        revision,
                        catalog_revision: catalog,
                        target: sift_protocol::SqlSymbolTarget::AtPosition { position },
                        cursor: None,
                        limit: Some(SEMANTIC_USAGE_LIMIT),
                    },
                )
                .await
            {
                Ok(page) => SemanticOutcome::Usages {
                    usages: page.usages,
                    is_complete: page.is_complete,
                },
                Err(error) => {
                    *catalog_revision = None;
                    SemanticOutcome::Failed(format!("finding usages failed: {error}"))
                }
            }
        }
        SemanticRequestKind::Rename { position, new_name } => {
            let catalog =
                current_catalog_revision(client, session, connection, catalog_revision).await;
            match client
                .prepare_semantic_refactor(
                    session,
                    connection,
                    document,
                    sift_protocol::PrepareSqlRefactorRequest {
                        revision,
                        catalog_revision: catalog,
                        refactor: sift_protocol::SqlRefactor::RenameSymbol { position, new_name },
                    },
                )
                .await
            {
                Ok(edit) => match workspace_edit_outcome(edit, document) {
                    SemanticOutcome::Edits { edits, warnings } => {
                        SemanticOutcome::RenamePreview { edits, warnings }
                    }
                    outcome => outcome,
                },
                Err(error) => SemanticOutcome::Failed(format!("preparing rename failed: {error}")),
            }
        }
        SemanticRequestKind::Outline { end } => {
            if end == 0 {
                return SemanticOutcome::Outline {
                    statements: Vec::new(),
                    symbols: Vec::new(),
                };
            }
            match client
                .select_semantic_statement(
                    session,
                    connection,
                    document,
                    sift_protocol::SelectStatementRequest {
                        revision,
                        cursor: 0,
                        selection: Some(sift_protocol::TextRange { start: 0, end }),
                    },
                )
                .await
            {
                Ok(selection) => SemanticOutcome::Outline {
                    statements: selection.statements,
                    symbols: selection.symbols,
                },
                Err(error) => {
                    SemanticOutcome::OutlineFailed(format!("loading query outline failed: {error}"))
                }
            }
        }
    }
}

/// Keep only the edits aimed at this editor's own document. A multi-document
/// `WorkspaceEdit` is reported rather than partially applied, because the
/// desktop has no way to edit another session's document on the user's behalf.
fn workspace_edit_outcome(
    edit: sift_protocol::WorkspaceEdit,
    document: sift_protocol::SemanticDocumentId,
) -> SemanticOutcome {
    let mut warnings = edit.warnings;
    let foreign = edit
        .documents
        .iter()
        .any(|target| target.document_id != document);
    if foreign {
        warnings.push("Edits for other documents were not applied.".into());
    }
    if !edit.is_complete {
        warnings.push("The server returned a partial edit.".into());
    }
    let edits = edit
        .documents
        .into_iter()
        .filter(|target| target.document_id == document)
        .flat_map(|target| target.edits)
        .collect();
    SemanticOutcome::Edits { edits, warnings }
}

async fn open_query_context(
    server: &DesktopServer,
    tenant_id: i64,
    profile_id: i64,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
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
    let metadata_connection = match client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id,
                profile_id,
            },
        )
        .await
    {
        Ok(connection) => connection.id,
        Err(error) => {
            let _ = client.close_session(session).await;
            return Err(format!("opening a metadata connection failed: {error}"));
        }
    };
    let plan_connection = match client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id,
                profile_id,
            },
        )
        .await
    {
        Ok(connection) => connection.id,
        Err(error) => {
            let _ = client.close_session(session).await;
            return Err(format!("opening a plan connection failed: {error}"));
        }
    };
    let semantic_connection = match client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id,
                profile_id,
            },
        )
        .await
    {
        Ok(connection) => connection.id,
        Err(error) => {
            let _ = client.close_session(session).await;
            return Err(format!("opening a semantic connection failed: {error}"));
        }
    };
    let (semantic, controls) = tokio::sync::mpsc::unbounded_channel();
    std::mem::drop(tokio::spawn(run_semantic_service(
        client.clone(),
        session,
        semantic_connection,
        controls,
        events.clone(),
    )));
    Ok(QueryContext {
        client,
        session,
        connection,
        transaction: None,
        metadata_connection,
        plan_connection,
        plan_lock: Arc::new(tokio::sync::Mutex::new(())),
        profile_id,
        connection_profile_id: Some(profile_id),
        tenant_id,
        semantic,
    })
}

async fn open_ad_hoc_query_context(
    server: &DesktopServer,
    tenant_id: i64,
    provider_id: sift_protocol::ProviderId,
    mut configuration: serde_json::Value,
    credentials: Option<serde_json::Value>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) -> Result<QueryContext, String> {
    if let (Some(configuration), Some(credentials)) = (
        configuration.as_object_mut(),
        credentials.and_then(|value| value.as_object().cloned()),
    ) {
        configuration.extend(credentials);
    }
    let spec: sift_protocol::ConnectionSpec = serde_json::from_value(configuration)
        .map_err(|error| format!("invalid ad-hoc connection configuration: {error}"))?;
    let client = server.client().await?;
    let session = client
        .open_session_for_tenant(Some("Sift desktop · ad-hoc".into()), Some(tenant_id))
        .await
        .map_err(|error| format!("opening an ad-hoc session failed: {error}"))?
        .id;
    let connection =
        open_explicit_connection(&client, session, &provider_id, &spec, "query").await?;
    let metadata_connection =
        match open_explicit_connection(&client, session, &provider_id, &spec, "metadata").await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = client.close_session(session).await;
                return Err(error);
            }
        };
    let plan_connection =
        match open_explicit_connection(&client, session, &provider_id, &spec, "plan").await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = client.close_session(session).await;
                return Err(error);
            }
        };
    let semantic_connection =
        match open_explicit_connection(&client, session, &provider_id, &spec, "semantic").await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = client.close_session(session).await;
                return Err(error);
            }
        };
    let (semantic, controls) = tokio::sync::mpsc::unbounded_channel();
    std::mem::drop(tokio::spawn(run_semantic_service(
        client.clone(),
        session,
        semantic_connection,
        controls,
        events.clone(),
    )));
    Ok(QueryContext {
        client,
        session,
        connection,
        transaction: None,
        metadata_connection,
        plan_connection,
        plan_lock: Arc::new(tokio::sync::Mutex::new(())),
        profile_id: 0,
        connection_profile_id: None,
        tenant_id,
        semantic,
    })
}

async fn open_explicit_connection(
    client: &Client,
    session: SessionId,
    provider_id: &sift_protocol::ProviderId,
    spec: &sift_protocol::ConnectionSpec,
    lane: &str,
) -> Result<ConnectionId, String> {
    client
        .open_connection(
            session,
            sift_protocol::OpenConnectionRequest {
                provider_id: provider_id.clone(),
                spec: spec.clone(),
            },
        )
        .await
        .map(|connection| connection.id)
        .map_err(|error| format!("opening ad-hoc {lane} connection failed: {error}"))
}

async fn load_capabilities(opened: &QueryContext) -> ExecutorEvent {
    let context = sift_protocol::OperationCapabilityContext {
        tenant_id: Some(opened.tenant_id),
        room_id: None,
        connection_profile_id: opened.connection_profile_id,
        session: Some(opened.session),
        connection: Some(opened.connection),
        transaction: None,
        workspace_id: None,
    };
    ExecutorEvent::CapabilitiesLoaded {
        profile_id: opened.profile_id,
        capabilities: opened
            .client
            .available_operations(&context)
            .await
            .map_err(|error| error.to_string()),
    }
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

enum RoomDocumentInput {
    Update(Vec<u8>),
    Flush(u64),
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
            tokio::sync::mpsc::UnboundedSender<RoomDocumentInput>,
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
                            let _ = sender.send(RoomDocumentInput::Update(update));
                        }
                    }
                    RoomDocumentCommand::Flush {
                        document_id,
                        generation,
                    } => {
                        if let Some((_, sender)) = documents.get(&document_id) {
                            let _ = sender.send(RoomDocumentInput::Flush(generation));
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
    mut updates: tokio::sync::mpsc::UnboundedReceiver<RoomDocumentInput>,
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
            input = updates.recv() => {
                let Some(input) = input else { return Ok(()) };
                match input {
                    RoomDocumentInput::Update(update) => {
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
                    RoomDocumentInput::Flush(generation) => {
                        let _ = events.send(RoomDocumentEvent::Flushed {
                            document_id: source.document_id,
                            generation,
                        });
                    }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .on_action(
                cx.listener(|window, _: &crate::OpenNewWindow, window_handle, cx| {
                    window.open_new_window(window_handle, cx)
                }),
            )
            .child(self.workspace.clone())
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
            kind: sift_workspace_ui::SavedServerKind::Hosted,
            ssh_state_dir: None,
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
    fn desktop_demo_root_replaces_legacy_demo_inventory_entries() {
        let root = |manifest_id: &str, name: &str| sift_workspace_ui::SavedInstanceRoot {
            manifest_id: manifest_id.into(),
            name: name.into(),
            root: std::path::PathBuf::from(format!("/tmp/{manifest_id}")),
        };
        let mut roots = vec![
            root("old-demo-one", "desktop-demo"),
            root("old-demo-two", "desktop-demo"),
            root("team", "Team"),
        ];

        reconcile_configured_root(&mut roots, root("stable-demo", "desktop-demo"));

        assert_eq!(roots.len(), 2);
        assert!(roots
            .iter()
            .any(|candidate| candidate.manifest_id == "stable-demo"));
        assert!(roots
            .iter()
            .any(|candidate| candidate.manifest_id == "team"));
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

    #[test]
    fn secondary_windows_have_independent_non_persisting_presentation_state() {
        let directory = tempfile::tempdir().unwrap();
        let local = DesktopServer::local(directory.path().join("runtime"));
        let services = WindowServices {
            store: Some(Arc::new(PresentationStore::new(
                directory.path().join("presentation.json"),
            ))),
            settings_store: Arc::new(SettingsStore::new(directory.path().join("settings.toml"))),
            settings: UserSettings::default(),
            runtime: Arc::new(tokio::runtime::Runtime::new().unwrap()),
            server: local.clone(),
            instance_store: InstanceStore::new(directory.path().join("instances.json")),
            credentials: DesktopCredentialStore,
            saved_servers: Vec::new(),
            instance_roots: Vec::new(),
            local_target: local,
            restored_profile_id: None,
        };
        let state = PresentationState::default();

        let secondary = services.for_secondary_window(&state);

        assert!(services.store.is_some());
        assert!(secondary.store.is_none());
        assert_eq!(secondary.server.instance().id, "local");
    }

    fn job(item_id: u64, text_revision: u64, request: SemanticRequestKind) -> SemanticControl {
        SemanticControl::Run(SemanticJob {
            item_id,
            text_revision,
            text: format!("select {text_revision}"),
            request,
        })
    }

    #[test]
    fn a_keystroke_burst_collapses_to_one_analysis_of_the_newest_text() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Analyze),
                job(1, 2, SemanticRequestKind::Analyze),
                job(1, 3, SemanticRequestKind::Analyze),
                job(2, 9, SemanticRequestKind::Analyze),
            ],
            &HashSet::new(),
            &events,
        );

        assert_eq!(kept.len(), 2);
        assert_eq!((kept[0].item_id, kept[0].text_revision), (1, 3));
        assert_eq!((kept[1].item_id, kept[1].text_revision), (2, 9));
        // Superseded analysis is silent; nothing was waiting on it.
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_superseded_interactive_request_is_reported_rather_than_dropped() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Complete { cursor: 3 }),
                job(1, 2, SemanticRequestKind::Analyze),
            ],
            &HashSet::new(),
            &events,
        );

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].request, SemanticRequestKind::Analyze);
        assert!(matches!(
            received.try_recv(),
            Ok(ExecutorEvent::Semantic {
                item_id: 1,
                text_revision: 1,
                ..
            })
        ));
    }

    #[test]
    fn work_for_a_closed_tab_is_abandoned() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let kept = admissible_jobs(
            vec![
                job(1, 1, SemanticRequestKind::Format { range: None }),
                SemanticControl::Close(1),
            ],
            &HashSet::from([1]),
            &events,
        );

        assert!(kept.is_empty());
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn foreign_documents_in_a_workspace_edit_are_not_applied() {
        let mine = sift_protocol::SemanticDocumentId(uuid::Uuid::from_u128(1));
        let theirs = sift_protocol::SemanticDocumentId(uuid::Uuid::from_u128(2));
        let outcome = workspace_edit_outcome(
            sift_protocol::WorkspaceEdit {
                documents: vec![
                    sift_protocol::DocumentEdit {
                        document_id: mine,
                        expected_revision: 1,
                        source_digest: "d".into(),
                        edits: vec![sift_protocol::TextEdit {
                            range: sift_protocol::TextRange { start: 0, end: 1 },
                            new_text: "A".into(),
                        }],
                    },
                    sift_protocol::DocumentEdit {
                        document_id: theirs,
                        expected_revision: 1,
                        source_digest: "d".into(),
                        edits: vec![sift_protocol::TextEdit {
                            range: sift_protocol::TextRange { start: 0, end: 1 },
                            new_text: "B".into(),
                        }],
                    },
                ],
                warnings: Vec::new(),
                is_complete: true,
                actual_range: None,
            },
            mine,
        );

        let SemanticOutcome::Edits { edits, warnings } = outcome else {
            panic!("formatting produces edits");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "A");
        assert_eq!(warnings.len(), 1);
    }
}
