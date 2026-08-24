use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const HISTORY_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ESTIMATED_PLAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ANALYZED_PLAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
    PresentationStore, Rect, ResultState, RoomDocumentCommand, RoomDocumentEvent, SemanticOutcome,
    SemanticRequestKind, SettingsStore, UserSettings, WorkspaceShell,
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
    tenant_id: i64,
    /// Semantic work runs on its own task; dropping this sender ends it and
    /// releases every server document it owns with the connection.
    semantic: tokio::sync::mpsc::UnboundedSender<SemanticControl>,
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
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            _ = health_tick.tick(), if context.is_some() => {
                let opened = context.as_ref().expect("guarded by context");
                let result = opened.client
                    .ping_connection(opened.session, opened.metadata_connection)
                    .await
                    .map(|_| ())
                    .map_err(|error| format!("database ping failed: {error}"));
                if events.send(ExecutorEvent::ConnectionHealth(result)).is_err() {
                    return;
                }
                continue;
            }
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
                let result = match context.as_mut() {
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
                };
                if events
                    .send(ExecutorEvent::TransactionChanged(result))
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
            ExecutorCommand::CreateSavedQuery { request } => {
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
                if events.send(ExecutorEvent::SavedQuerySaved(result)).is_err() {
                    return;
                }
            }
            ExecutorCommand::UpdateSavedQuery { id, request } => {
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
                if events.send(ExecutorEvent::SavedQuerySaved(result)).is_err() {
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
                        .map_err(|error| format!("applying result edit failed: {error}")),
                    None => Err("Connect to this table before applying an edit".into()),
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
            ExecutorCommand::CapturePlan { item_id, sql } => {
                let result = match context.as_ref() {
                    Some(opened) => capture_plan(opened, sql).await,
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
                            outcome: Box::new(SemanticOutcome::Failed(
                                "Not connected — SQL analysis needs a connection.".into(),
                            )),
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
                    params: Vec::new(),
                    include_raw_response: false,
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
    let result = context
        .client
        .plan_captures(
            TenantId(tenant_id),
            sift_protocol::ListPlanCapturesRequest {
                source_digest: Some(state.source_digest),
                limit: Some(100),
                ..Default::default()
            },
        )
        .await
        .map(|page| {
            page.items
                .into_iter()
                .filter(|capture| capture.statement_fingerprint == fingerprint)
                .collect()
        })
        .map_err(|error| format!("loading plan captures failed: {error}"));
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
        transaction,
        item_id,
        execution_id,
        sql,
        params,
    } = run;
    let started = tokio::select! {
        stream = client.start_query_stream_with(
            session,
            connection,
            sql,
            params,
            transaction.map(|transaction| sift_protocol::TxHandleRef {
                tx_id: transaction.tx_id,
                connection: transaction.connection,
                mode: transaction.mode,
            }),
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
    let mut kept = Vec::with_capacity(jobs.len());
    for job in jobs.drain(..).rev() {
        let current = newest.get(&job.item_id).copied() == Some(job.text_revision);
        let duplicate_analyze =
            job.request == SemanticRequestKind::Analyze && !analyzed.insert(job.item_id);
        if current && !duplicate_analyze {
            kept.push(job);
            continue;
        }
        if job.request != SemanticRequestKind::Analyze {
            let _ = events.send(ExecutorEvent::Semantic {
                item_id: job.item_id,
                text_revision: job.text_revision,
                outcome: Box::new(SemanticOutcome::Failed(
                    "Buffer changed before the request ran.".into(),
                )),
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
        tenant_id,
        semantic,
    })
}

async fn load_capabilities(opened: &QueryContext) -> ExecutorEvent {
    let context = sift_protocol::OperationCapabilityContext {
        tenant_id: Some(opened.tenant_id),
        room_id: None,
        connection_profile_id: Some(opened.profile_id),
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
