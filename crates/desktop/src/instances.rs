use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};

use keyring::{Entry, Error as KeyringError};
use sift_client_sdk::SessionTokenProvider;
use sift_protocol::{AuthClientKind, AuthTokensResponse, PasswordLoginRequest};
use sift_workspace_ui::{
    InstanceCommand, InstanceConfigurationPresentation, InstanceCredentialKind,
    InstanceCredentialPresentation, InstanceManagerEvent, InstancePlanPresentation,
    SavedInstanceRoot, SavedServerKind, SavedServerProfile,
};

use crate::app::DesktopServer;
use crate::config::{validate_base_url, validate_token};

const PROFILE_VERSION: u32 = 1;
const KEYCHAIN_SERVICE: &str = "sift-desktop";

#[derive(Clone)]
pub struct InstanceStore {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredInstances {
    version: u32,
    profiles: Vec<SavedServerProfile>,
    #[serde(default)]
    roots: Vec<SavedInstanceRoot>,
}

impl InstanceStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn load(&self) -> Vec<SavedServerProfile> {
        self.load_stored().profiles
    }

    pub fn load_roots(&self) -> Vec<SavedInstanceRoot> {
        self.load_stored().roots
    }

    fn load_stored(&self) -> StoredInstances {
        let mut stored = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StoredInstances>(&bytes).ok())
            .filter(|stored| stored.version == PROFILE_VERSION)
            .unwrap_or(StoredInstances {
                version: PROFILE_VERSION,
                profiles: Vec::new(),
                roots: Vec::new(),
            });
        for profile in &mut stored.profiles {
            profile.has_saved_token = false;
        }
        stored
    }

    pub fn save(&self, profiles: &[SavedServerProfile]) -> Result<(), String> {
        let roots = self.load_stored().roots;
        self.save_inventory(profiles, &roots)
    }

    fn save_inventory(
        &self,
        profiles: &[SavedServerProfile],
        roots: &[SavedInstanceRoot],
    ) -> Result<(), String> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| "instance profile write lock poisoned".to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("creating instance profile directory: {error}"))?;
        }
        let bytes = serde_json::to_vec_pretty(&StoredInstances {
            version: PROFILE_VERSION,
            profiles: profiles.to_vec(),
            roots: roots.to_vec(),
        })
        .map_err(|error| format!("encoding instance profiles: {error}"))?;
        let temporary = self.path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("opening temporary instance profile file: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("writing instance profiles: {error}"))?;
        drop(file);
        #[cfg(windows)]
        if self.path.exists() {
            std::fs::remove_file(&self.path)
                .map_err(|error| format!("replacing instance profiles: {error}"))?;
        }
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("committing instance profiles: {error}"))
    }

    pub(crate) fn save_roots(
        &self,
        profiles: &[SavedServerProfile],
        roots: &[SavedInstanceRoot],
    ) -> Result<(), String> {
        self.save_inventory(profiles, roots)
    }

    #[cfg(test)]
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[derive(Clone, Default)]
pub struct DesktopCredentialStore;

impl DesktopCredentialStore {
    async fn get(&self, profile_id: &str) -> Result<Option<String>, String> {
        let profile_id = profile_id.to_owned();
        keychain_blocking(move || match entry(&profile_id)?.get_password() {
            Ok(token) => Ok(Some(validate_token(token)?)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(error)),
        })
        .await
    }

    async fn put(&self, profile_id: &str, token: &str) -> Result<(), String> {
        let profile_id = profile_id.to_owned();
        let token = token.to_owned();
        keychain_blocking(move || {
            entry(&profile_id)?
                .set_password(&token)
                .map_err(keychain_error)
        })
        .await
    }

    async fn delete(&self, profile_id: &str) -> Result<(), String> {
        let profile_id = profile_id.to_owned();
        keychain_blocking(move || match entry(&profile_id)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        })
        .await
    }

    async fn get_session(&self, profile_id: &str) -> Result<Option<SessionTokenProvider>, String> {
        let profile_id = profile_id.to_owned();
        keychain_blocking(move || match auth_entry(&profile_id)?.get_password() {
            Ok(encoded) => serde_json::from_str::<AuthTokensResponse>(&encoded)
                .map(SessionTokenProvider::new)
                .map(Some)
                .map_err(|error| format!("decoding session from OS keychain: {error}")),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(error)),
        })
        .await
    }

    async fn put_session(
        &self,
        profile_id: &str,
        provider: &SessionTokenProvider,
    ) -> Result<(), String> {
        let profile_id = profile_id.to_owned();
        let encoded = serde_json::to_string(&provider.snapshot().await)
            .map_err(|error| format!("encoding session for OS keychain: {error}"))?;
        keychain_blocking(move || {
            auth_entry(&profile_id)?
                .set_password(&encoded)
                .map_err(keychain_error)
        })
        .await
    }

    async fn delete_session(&self, profile_id: &str) -> Result<(), String> {
        let profile_id = profile_id.to_owned();
        keychain_blocking(move || match auth_entry(&profile_id)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        })
        .await
    }
}

fn entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, &format!("server:{profile_id}")).map_err(keychain_error)
}

fn auth_entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, &format!("auth:{profile_id}")).map_err(keychain_error)
}

fn keychain_error(error: KeyringError) -> String {
    format!("OS keychain: {error}")
}

async fn keychain_blocking<T>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("OS keychain task failed: {error}"))?
}

pub async fn annotate_saved_tokens(
    profiles: &mut [SavedServerProfile],
    credentials: &DesktopCredentialStore,
) {
    for profile in profiles {
        profile.has_saved_token = credentials.get(&profile.id).await.ok().flatten().is_some();
    }
}

pub struct InstanceManagerChannels {
    pub commands: tokio::sync::mpsc::UnboundedReceiver<InstanceCommand>,
    pub events: tokio::sync::mpsc::UnboundedSender<InstanceManagerEvent>,
    pub targets: tokio::sync::watch::Sender<DesktopServer>,
}

pub async fn run_instance_manager(
    store: InstanceStore,
    credentials: DesktopCredentialStore,
    mut profiles: Vec<SavedServerProfile>,
    mut roots: Vec<SavedInstanceRoot>,
    mut channels: InstanceManagerChannels,
    local_target: DesktopServer,
    restored_profile_id: Option<String>,
) {
    // Started configured roots remain alive independently of which one the
    // window is currently connected to. This lets one desktop supervise
    // several isolated auto-loopback instances without conflating UI state.
    let mut configured_targets = std::collections::HashMap::new();
    let mut ssh_supervisor: Option<tokio::task::JoinHandle<()>> = None;
    annotate_saved_tokens(&mut profiles, &credentials).await;
    if let Some(profile) = restored_profile_id
        .as_deref()
        .and_then(|id| profiles.iter().find(|profile| profile.id == id))
        .cloned()
    {
        if profile.kind == SavedServerKind::Ssh {
            if let Ok((_, task)) = connect_ssh(
                &store,
                &mut profiles,
                &channels.targets,
                Some(profile.id.clone()),
                profile.name,
                profile.base_url,
                profile
                    .ssh_state_dir
                    .unwrap_or_else(|| ".local/state/sift/remote".into()),
            )
            .await
            {
                ssh_supervisor = Some(task);
            }
        } else {
            let session = credentials.get_session(&profile.id).await.ok().flatten();
            let token = if session.is_none() && profile.has_saved_token {
                credentials.get(&profile.id).await.ok().flatten()
            } else {
                None
            };
            let target = DesktopServer::remote(profile, token);
            let target = match session {
                Some(session) => target.with_session_tokens(session).unwrap_or(target),
                None => target,
            };
            let _ = channels.targets.send(target);
        }
    }
    if channels
        .events
        .send(InstanceManagerEvent::Profiles(profiles.clone()))
        .is_err()
    {
        return;
    }
    let _ = channels
        .events
        .send(InstanceManagerEvent::Roots(roots.clone()));
    while let Some(command) = channels.commands.recv().await {
        let (authentication, result) = match command {
            InstanceCommand::UseLocal => {
                if let Some(task) = ssh_supervisor.take() {
                    task.abort();
                }
                (false, connect_local(&channels.targets, &local_target).await)
            }
            InstanceCommand::Connect {
                profile_id,
                name,
                base_url,
                bearer_token,
                remember_token,
            } => {
                if let Some(task) = ssh_supervisor.take() {
                    task.abort();
                }
                let _ = channels.events.send(InstanceManagerEvent::Testing);
                (
                    false,
                    connect(
                        &store,
                        &credentials,
                        &mut profiles,
                        &channels.targets,
                        profile_id,
                        name,
                        base_url,
                        bearer_token,
                        remember_token,
                    )
                    .await,
                )
            }
            InstanceCommand::ConnectSsh {
                profile_id,
                name,
                destination,
                state_dir,
            } => {
                if let Some(task) = ssh_supervisor.take() {
                    task.abort();
                }
                let _ = channels.events.send(InstanceManagerEvent::Testing);
                let result = connect_ssh(
                    &store,
                    &mut profiles,
                    &channels.targets,
                    profile_id,
                    name,
                    destination,
                    state_dir,
                )
                .await
                .map(|(outcome, task)| {
                    ssh_supervisor = Some(task);
                    outcome
                });
                (false, result)
            }
            InstanceCommand::Forget { profile_id } => (
                false,
                forget(&store, &credentials, &mut profiles, &profile_id).await,
            ),
            InstanceCommand::ForgetRoot { root } => {
                let result = forget_root(
                    &store,
                    &profiles,
                    &mut roots,
                    &mut configured_targets,
                    &channels.targets,
                    &local_target,
                    &root,
                )
                .and_then(|outcome| {
                    channels
                        .events
                        .send(InstanceManagerEvent::Roots(roots.clone()))
                        .map_err(|_| "desktop window closed".to_string())?;
                    Ok(outcome)
                });
                (false, result)
            }
            InstanceCommand::InspectRoot { root } => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::InstanceOperationPending {
                        message: "Validating instance configuration…".into(),
                    });
                let result = inspect_root(&root, None).await.and_then(|plan| {
                    remember_root(&store, &profiles, &mut roots, &plan)?;
                    channels
                        .events
                        .send(InstanceManagerEvent::Roots(roots.clone()))
                        .map_err(|_| "desktop window closed".to_string())?;
                    channels
                        .events
                        .send(InstanceManagerEvent::InstancePlan(Box::new(plan)))
                        .map_err(|_| "desktop window closed".to_string())?;
                    Ok(ManagerOutcome::None)
                });
                (false, result)
            }
            InstanceCommand::ApplyRoot {
                root,
                allow_destroy,
            } => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::InstanceOperationPending {
                        message: "Applying instance generation…".into(),
                    });
                let result = apply_root(&root, allow_destroy).await.and_then(|plan| {
                    remember_root(&store, &profiles, &mut roots, &plan)?;
                    channels
                        .events
                        .send(InstanceManagerEvent::Roots(roots.clone()))
                        .map_err(|_| "desktop window closed".to_string())?;
                    channels
                        .events
                        .send(InstanceManagerEvent::InstancePlan(Box::new(plan)))
                        .map_err(|_| "desktop window closed".to_string())?;
                    Ok(ManagerOutcome::None)
                });
                (false, result)
            }
            InstanceCommand::ImportRootCredential {
                root,
                slot,
                kind,
                secret,
            } => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::InstanceOperationPending {
                        message: format!("Importing credential slot {slot}…"),
                    });
                let result = match import_root_credential(&root, &slot, kind, secret).await {
                    Ok(()) => inspect_root(&root, Some("Credential imported".into())).await,
                    Err(error) => Err(error),
                }
                .and_then(|plan| {
                    channels
                        .events
                        .send(InstanceManagerEvent::InstancePlan(Box::new(plan)))
                        .map_err(|_| "desktop window closed".to_string())?;
                    Ok(ManagerOutcome::None)
                });
                (false, result)
            }
            InstanceCommand::StartRoot { root } => {
                let _ = channels.events.send(InstanceManagerEvent::Testing);
                let result = connect_root(&channels.targets, &mut configured_targets, &root).await;
                (false, result)
            }
            InstanceCommand::PrepareRootConfiguration { root } => {
                let result = prepare_root_configuration(&root).and_then(|configuration| {
                    channels
                        .events
                        .send(InstanceManagerEvent::InstanceConfiguration(Box::new(
                            configuration,
                        )))
                        .map_err(|_| "desktop window closed".to_string())?;
                    Ok(ManagerOutcome::None)
                });
                (false, result)
            }
            InstanceCommand::OpenRootConfiguration { root } => {
                let result = open_root_configuration(&root).and_then(|configuration| {
                    channels
                        .events
                        .send(InstanceManagerEvent::InstanceConfiguration(Box::new(
                            configuration,
                        )))
                        .map_err(|_| "desktop window closed".to_string())?;
                    Ok(ManagerOutcome::None)
                });
                (false, result)
            }
            InstanceCommand::OpenCurrentConfiguration => {
                let result = open_current_configuration(&channels.targets)
                    .await
                    .and_then(|configuration| {
                        channels
                            .events
                            .send(InstanceManagerEvent::InstanceConfiguration(Box::new(
                                configuration,
                            )))
                            .map_err(|_| "desktop window closed".to_string())?;
                        Ok(ManagerOutcome::None)
                    });
                (false, result)
            }
            InstanceCommand::SaveConfiguration {
                root,
                manifest,
                expected_source_revision,
                is_new,
            } => {
                let result = match save_configuration(
                    &channels.targets,
                    root.as_deref(),
                    manifest,
                    expected_source_revision,
                    is_new,
                )
                .await
                {
                    Ok(configuration) => {
                        publish_saved_configuration(
                            &store,
                            &profiles,
                            &mut roots,
                            &channels.events,
                            configuration,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                (false, result)
            }
            InstanceCommand::SignInWithPassword { username, password } => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::AuthenticationPending);
                (
                    true,
                    sign_in_with_password(&channels.targets, &credentials, username, password)
                        .await,
                )
            }
            InstanceCommand::SignInWithGithub => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::AuthenticationPending);
                (
                    true,
                    sign_in_with_github(&channels.targets, &channels.events, &credentials).await,
                )
            }
            InstanceCommand::RefreshSession => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::AuthenticationPending);
                (true, refresh_session(&channels.targets).await)
            }
            InstanceCommand::SignOut { everywhere } => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::AuthenticationPending);
                (
                    true,
                    sign_out(&channels.targets, &credentials, everywhere).await,
                )
            }
        };
        match result {
            Ok(ManagerOutcome::Connected(name)) => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::Profiles(profiles.clone()));
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::Connected { name });
            }
            Ok(ManagerOutcome::ProfilesChanged) => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::Profiles(profiles.clone()));
            }
            Ok(ManagerOutcome::Authenticated(display_name)) => {
                let _ = channels
                    .events
                    .send(InstanceManagerEvent::Authenticated { display_name });
            }
            Ok(ManagerOutcome::SignedOut) => {
                let _ = channels.events.send(InstanceManagerEvent::SignedOut);
            }
            Ok(ManagerOutcome::None) => {}
            Err(message) => {
                let event = if authentication {
                    InstanceManagerEvent::AuthenticationFailed { message }
                } else {
                    InstanceManagerEvent::Failed { message }
                };
                let _ = channels.events.send(event);
            }
        }
    }
}

async fn connect_local(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    target: &DesktopServer,
) -> Result<ManagerOutcome, String> {
    let client = target.client().await?;
    test_client(&client, "local server").await?;
    targets
        .send(target.clone())
        .map_err(|_| "desktop server supervisor stopped".to_string())?;
    Ok(ManagerOutcome::Connected("Local Sift".into()))
}

enum ManagerOutcome {
    Connected(String),
    ProfilesChanged,
    Authenticated(String),
    SignedOut,
    None,
}

fn remember_root(
    store: &InstanceStore,
    profiles: &[SavedServerProfile],
    roots: &mut Vec<SavedInstanceRoot>,
    plan: &InstancePlanPresentation,
) -> Result<(), String> {
    let saved = SavedInstanceRoot {
        manifest_id: plan.manifest_id.clone(),
        name: plan.name.clone(),
        root: plan.root.clone(),
    };
    if let Some(index) = roots
        .iter()
        .position(|root| root.manifest_id == saved.manifest_id)
    {
        roots[index] = saved;
    } else {
        roots.push(saved);
    }
    roots.sort_by(|left, right| left.name.cmp(&right.name));
    store.save_roots(profiles, roots)
}

fn forget_root(
    store: &InstanceStore,
    profiles: &[SavedServerProfile],
    roots: &mut Vec<SavedInstanceRoot>,
    configured_targets: &mut std::collections::HashMap<
        PathBuf,
        (DesktopServer, crate::local_server::LocalServerLease),
    >,
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    local_target: &DesktopServer,
    root: &std::path::Path,
) -> Result<ManagerOutcome, String> {
    let canonical = std::fs::canonicalize(root).ok();
    roots.retain(|saved| {
        saved.root != root
            && match &canonical {
                Some(canonical) => std::fs::canonicalize(&saved.root)
                    .map(|saved| saved != *canonical)
                    .unwrap_or(true),
                None => true,
            }
    });
    store.save_roots(profiles, roots)?;
    if let Some(canonical) = &canonical {
        configured_targets.remove(canonical);
    }
    let active_is_removed = targets
        .borrow()
        .configured_root()
        .is_some_and(|active| active == root || canonical.as_deref() == Some(active));
    if active_is_removed {
        targets
            .send(local_target.clone())
            .map_err(|_| "desktop server supervisor stopped".to_string())?;
        Ok(ManagerOutcome::Connected("Local Sift".into()))
    } else {
        Ok(ManagerOutcome::None)
    }
}

fn credential_kind(kind: sift_instance_config::CredentialKind) -> InstanceCredentialKind {
    match kind {
        sift_instance_config::CredentialKind::GithubOauthClientSecret => {
            InstanceCredentialKind::GithubOauthClientSecret
        }
        sift_instance_config::CredentialKind::Postgres => InstanceCredentialKind::Postgres,
        sift_instance_config::CredentialKind::SqlServer => InstanceCredentialKind::SqlServer,
    }
}

fn prepare_root_configuration(
    root: &std::path::Path,
) -> Result<InstanceConfigurationPresentation, String> {
    for name in ["sift.toml", "sift.lock"] {
        if root
            .join(name)
            .try_exists()
            .map_err(|error| format!("checking instance folder failed: {error}"))?
        {
            return Err(format!(
                "{} already exists; open that instance instead",
                root.join(name).display()
            ));
        }
    }
    let source = include_str!("../../../examples/reproducible-instance/sift.toml")
        .replace(
            "b654b918-b1f1-4d70-924d-e4c1014f482f",
            &uuid::Uuid::new_v4().to_string(),
        )
        .replace("name = \"demo-sift\"", "name = \"new-sift\"");
    Ok(InstanceConfigurationPresentation {
        root: Some(root.to_path_buf()),
        manifest: source,
        source_revision: None,
        name: "New Sift Instance".into(),
        is_new: true,
    })
}

fn open_root_configuration(
    root: &std::path::Path,
) -> Result<InstanceConfigurationPresentation, String> {
    let document = sift_server::instance_configuration::read(root)
        .map_err(|error| format!("opening sift.toml failed: {error:#}"))?;
    Ok(configuration_presentation(
        Some(root.to_path_buf()),
        document,
        false,
    ))
}

async fn open_current_configuration(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
) -> Result<InstanceConfigurationPresentation, String> {
    let target = { targets.borrow().clone() };
    if matches!(target, DesktopServer::Local(_)) {
        return Err(
            "Bundled Local Sift has no sift.toml. Create a new instance or open an existing instance root instead."
                .into(),
        );
    }
    let document = target
        .client()
        .await?
        .instance_configuration()
        .await
        .map_err(|error| format!("opening current instance sift.toml failed: {error}"))?;
    Ok(configuration_presentation(None, document, false))
}

async fn save_configuration(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    root: Option<&std::path::Path>,
    manifest: String,
    expected_source_revision: Option<String>,
    is_new: bool,
) -> Result<InstanceConfigurationPresentation, String> {
    let document = if let Some(root) = root {
        if is_new {
            sift_server::instance_configuration::create(root, &manifest)
                .map_err(|error| format!("creating instance failed: {error:#}"))?
        } else {
            sift_server::instance_configuration::update(
                root,
                &manifest,
                expected_source_revision.as_deref(),
            )
            .map_err(|error| format!("saving sift.toml failed: {error:#}"))?
        }
    } else {
        let expected_source_revision = expected_source_revision
            .ok_or_else(|| "current instance configuration has no source revision".to_string())?;
        let target = { targets.borrow().clone() };
        target
            .client()
            .await?
            .update_instance_configuration(sift_client_sdk::UpdateInstanceConfigurationRequest {
                manifest,
                expected_source_revision,
            })
            .await
            .map_err(|error| format!("saving current instance sift.toml failed: {error}"))?
    };
    Ok(configuration_presentation(
        root.map(std::path::Path::to_path_buf),
        document,
        false,
    ))
}

fn configuration_presentation(
    root: Option<PathBuf>,
    document: sift_client_sdk::InstanceConfigurationDocument,
    is_new: bool,
) -> InstanceConfigurationPresentation {
    InstanceConfigurationPresentation {
        root,
        manifest: document.manifest,
        source_revision: Some(document.source_revision),
        name: document.name,
        is_new,
    }
}

async fn publish_saved_configuration(
    store: &InstanceStore,
    profiles: &[SavedServerProfile],
    roots: &mut Vec<SavedInstanceRoot>,
    events: &tokio::sync::mpsc::UnboundedSender<InstanceManagerEvent>,
    configuration: InstanceConfigurationPresentation,
) -> Result<ManagerOutcome, String> {
    if let Some(root) = configuration.root.as_deref() {
        let plan = inspect_root(
            root,
            Some("sift.toml saved; apply the plan to activate it".into()),
        )
        .await?;
        remember_root(store, profiles, roots, &plan)?;
        let _ = events.send(InstanceManagerEvent::Roots(roots.clone()));
        let _ = events.send(InstanceManagerEvent::InstancePlan(Box::new(plan)));
    }
    events
        .send(InstanceManagerEvent::InstanceConfiguration(Box::new(
            configuration,
        )))
        .map_err(|_| "desktop window closed".to_string())?;
    Ok(ManagerOutcome::None)
}

async fn inspect_root(
    root: &std::path::Path,
    last_apply: Option<String>,
) -> Result<InstancePlanPresentation, String> {
    let instance = sift_server::instance_runtime::InstanceRoot::open(root)
        .map_err(|error| format!("validating instance root failed: {error:#}"))?;
    let static_plan = instance
        .static_plan()
        .map_err(|error| format!("planning instance failed: {error:#}"))?;
    let state_dir = instance.default_state_dir();
    let generations = instance
        .generations(&state_dir)
        .map_err(|error| format!("reading instance generations failed: {error:#}"))?;
    let current = generations.iter().find(|generation| generation.current);
    let drifted = current.is_none_or(|generation| {
        generation.record.configuration_digest != static_plan.configuration_digest
            || generation.record.lock_digest != static_plan.lock_digest
    });

    let credentials = if current.is_some() && !drifted {
        let (_, _, config) = sift_server::instance_runtime::load_current_config(root, None)
            .map_err(|error| format!("loading applied instance failed: {error:#}"))?;
        sift_server::instance_runtime::ensure_file_secret_key(&config)
            .map_err(|error| format!("preparing instance secret store failed: {error:#}"))?;
        let store = sift_server::metadata_runtime::build_metadata_store(&config)
            .map_err(|error| format!("opening instance metadata failed: {error:#}"))?
            .ok_or_else(|| "instance metadata is disabled".to_string())?;
        store
            .verified_instance_credential_status()
            .await
            .map_err(|error| format!("checking instance credentials failed: {error}"))?
            .into_iter()
            .map(|credential| InstanceCredentialPresentation {
                slot: credential.slot,
                consumer: credential.consumers.join(", "),
                kind: credential_kind(credential.kind),
                readiness: match credential.readiness {
                    sift_metadata::CredentialReadiness::Missing => "missing",
                    sift_metadata::CredentialReadiness::Ready => "ready",
                    sift_metadata::CredentialReadiness::Invalid => "invalid",
                }
                .into(),
            })
            .collect()
    } else {
        static_plan
            .required_credentials
            .iter()
            .map(|credential| InstanceCredentialPresentation {
                slot: credential.slot.clone(),
                consumer: credential.consumer.clone(),
                kind: credential_kind(credential.kind),
                readiness: "apply first".into(),
            })
            .collect()
    };

    Ok(InstancePlanPresentation {
        root: instance.root,
        manifest_id: instance.manifest.manifest_id.to_string(),
        name: instance.manifest.name,
        deployment: format!("{:?}", instance.manifest.server.deployment).to_lowercase(),
        bind: instance.manifest.server.bind,
        configuration_digest: static_plan.configuration_digest,
        lock_digest: static_plan.lock_digest,
        principals: static_plan.resources.principals,
        tenants: static_plan.resources.tenants,
        memberships: static_plan.resources.memberships,
        connections: static_plan.resources.connections,
        extensions: static_plan.resources.extensions,
        warnings: static_plan.warnings,
        credentials,
        current_generation: current.map(|generation| generation.record.generation),
        generation_count: generations.len(),
        drifted,
        last_apply,
        destroy_confirmation_required: false,
    })
}

async fn apply_root(
    root: &std::path::Path,
    allow_destroy: bool,
) -> Result<InstancePlanPresentation, String> {
    let instance = sift_server::instance_runtime::InstanceRoot::open(root)
        .map_err(|error| format!("validating instance root failed: {error:#}"))?;
    let state_dir = instance.default_state_dir();
    match instance.apply(&state_dir, allow_destroy).await {
        Ok(report) => {
            let metadata = report.metadata.as_ref();
            let summary = format!(
                "Generation {} · {} created · {} updated · {} deleted",
                report.generation,
                metadata.map_or(0, |summary| summary.created),
                metadata.map_or(0, |summary| summary.updated),
                metadata.map_or(0, |summary| summary.deleted),
            );
            inspect_root(root, Some(summary)).await
        }
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("destroy approval") || message.contains("allow-destroy") {
                let mut plan = inspect_root(root, Some(message)).await?;
                plan.destroy_confirmation_required = true;
                Ok(plan)
            } else {
                Err(format!("applying instance failed: {message}"))
            }
        }
    }
}

async fn import_root_credential(
    root: &std::path::Path,
    slot: &str,
    kind: InstanceCredentialKind,
    secret: String,
) -> Result<(), String> {
    let (_, _, config) = sift_server::instance_runtime::load_current_config(root, None)
        .map_err(|error| format!("loading applied instance failed: {error:#}"))?;
    sift_server::instance_runtime::ensure_file_secret_key(&config)
        .map_err(|error| format!("preparing instance secret store failed: {error:#}"))?;
    let _maintenance = sift_server::runtime::acquire_maintenance_exclusive(&config)
        .map_err(|error| format!("stop the instance before importing credentials: {error:#}"))?;
    let store = sift_server::metadata_runtime::build_metadata_store(&config)
        .map_err(|error| format!("opening instance metadata failed: {error:#}"))?
        .ok_or_else(|| "instance metadata is disabled".to_string())?;
    let field = match kind {
        InstanceCredentialKind::GithubOauthClientSecret => "client_secret",
        InstanceCredentialKind::Postgres | InstanceCredentialKind::SqlServer => "password",
    };
    let mut credential = serde_json::Map::new();
    credential.insert(field.into(), serde_json::Value::String(secret));
    store
        .import_instance_credential(slot, &serde_json::Value::Object(credential))
        .await
        .map_err(|error| format!("importing credential failed: {error}"))
}

async fn connect_root(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    configured_targets: &mut std::collections::HashMap<
        PathBuf,
        (DesktopServer, crate::local_server::LocalServerLease),
    >,
    root: &std::path::Path,
) -> Result<ManagerOutcome, String> {
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("canonicalizing instance root failed: {error}"))?;
    let target = match configured_targets.get(&root) {
        Some((target, _lease)) => target.clone(),
        None => DesktopServer::configured(root.clone())?,
    };
    let name = target.instance().name;
    let client = target.client().await?;
    test_client(&client, "configured instance").await?;
    if let std::collections::hash_map::Entry::Vacant(entry) = configured_targets.entry(root) {
        let lease = target
            .acquire_local_lease()
            .ok_or_else(|| "configured instance did not provide a process lease".to_string())?;
        entry.insert((target.clone(), lease));
    }
    targets
        .send(target)
        .map_err(|_| "desktop server supervisor stopped".to_string())?;
    Ok(ManagerOutcome::Connected(name))
}

async fn sign_in_with_password(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    credentials: &DesktopCredentialStore,
    username: String,
    password: String,
) -> Result<ManagerOutcome, String> {
    let server = targets.borrow().clone();
    let anonymous = server.without_authentication()?;
    let client = anonymous.client().await?;
    let provider = client
        .password_login(PasswordLoginRequest {
            username,
            password,
            client_kind: AuthClientKind::Native,
            client_label: Some("Sift Desktop".into()),
        })
        .await
        .map_err(|error| format!("signing in failed: {error}"))?;
    activate_session(targets, credentials, &server, provider).await
}

async fn sign_in_with_github(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    events: &tokio::sync::mpsc::UnboundedSender<InstanceManagerEvent>,
    credentials: &DesktopCredentialStore,
) -> Result<ManagerOutcome, String> {
    let server = targets.borrow().clone();
    let anonymous = server.without_authentication()?;
    let client = anonymous.client().await?;
    let start = client
        .github_native_start()
        .await
        .map_err(|error| format!("starting GitHub sign in failed: {error}"))?;
    events
        .send(InstanceManagerEvent::GithubAuthorization {
            url: start.authorization_url,
        })
        .map_err(|_| "desktop window closed during GitHub sign in".to_string())?;

    for _ in 0..300 {
        match client
            .github_native_exchange(start.handoff_token.clone())
            .await
        {
            Ok(provider) => {
                return activate_session(targets, credentials, &server, provider).await;
            }
            Err(sift_client_sdk::Error::Server { status, .. })
                if status == reqwest::StatusCode::UNAUTHORIZED =>
            {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(error) => return Err(format!("completing GitHub sign in failed: {error}")),
        }
    }
    Err("GitHub sign in timed out; start the flow again".into())
}

async fn activate_session(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    credentials: &DesktopCredentialStore,
    server: &DesktopServer,
    provider: sift_client_sdk::SessionTokenProvider,
) -> Result<ManagerOutcome, String> {
    let authenticated = server.with_session_tokens(provider.clone())?;
    let identity = authenticated
        .client()
        .await?
        .whoami()
        .await
        .map_err(|error| format!("loading the signed-in account failed: {error}"))?;
    let profile_id = auth_profile_id(server)?;
    credentials.put_session(&profile_id, &provider).await?;
    targets
        .send(authenticated)
        .map_err(|_| "desktop server supervisor stopped".to_string())?;
    Ok(ManagerOutcome::Authenticated(
        identity.principal.display_name,
    ))
}

async fn sign_out(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    credentials: &DesktopCredentialStore,
    everywhere: bool,
) -> Result<ManagerOutcome, String> {
    let server = targets.borrow().clone();
    let profile_id = auth_profile_id(&server)?;
    let client = server.client().await?;
    if everywhere {
        client.logout_all().await
    } else {
        client.logout().await
    }
    .map_err(|error| format!("signing out failed: {error}"))?;
    credentials.delete_session(&profile_id).await?;
    targets
        .send(server.without_authentication()?)
        .map_err(|_| "desktop server supervisor stopped".to_string())?;
    Ok(ManagerOutcome::SignedOut)
}

async fn refresh_session(
    targets: &tokio::sync::watch::Sender<DesktopServer>,
) -> Result<ManagerOutcome, String> {
    let server = targets.borrow().clone();
    let client = server.client().await?;
    client
        .refresh_session()
        .await
        .map_err(|error| format!("refreshing the account session failed: {error}"))?;
    let identity = client
        .whoami()
        .await
        .map_err(|error| format!("loading the refreshed account failed: {error}"))?;
    Ok(ManagerOutcome::Authenticated(
        identity.principal.display_name,
    ))
}

fn auth_profile_id(server: &DesktopServer) -> Result<String, String> {
    server
        .instance()
        .id
        .strip_prefix("hosted:")
        .map(str::to_owned)
        .ok_or_else(|| "Local Sift uses its built-in identity".into())
}

#[allow(clippy::too_many_arguments)]
async fn connect(
    store: &InstanceStore,
    credentials: &DesktopCredentialStore,
    profiles: &mut Vec<SavedServerProfile>,
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    requested_id: Option<String>,
    name: String,
    base_url: String,
    bearer_token: Option<String>,
    remember_token: bool,
) -> Result<ManagerOutcome, String> {
    let name = name.trim().to_owned();
    if name.is_empty() || name.len() > 120 {
        return Err("server name must be between 1 and 120 characters".into());
    }
    let base_url = validate_base_url(&base_url)?;
    let existing = requested_id
        .as_deref()
        .and_then(|id| profiles.iter().find(|profile| profile.id == id));
    let had_saved_token = existing.is_some_and(|profile| profile.has_saved_token);
    let profile_id = existing
        .map(|profile| profile.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session = if bearer_token.is_none() {
        credentials.get_session(&profile_id).await?
    } else {
        None
    };
    let token = match bearer_token {
        Some(token) => Some(validate_token(token)?),
        None if session.is_none() && had_saved_token => credentials.get(&profile_id).await?,
        None => None,
    };
    let profile = SavedServerProfile {
        id: profile_id.clone(),
        name: name.clone(),
        base_url,
        kind: SavedServerKind::Hosted,
        ssh_state_dir: None,
        has_saved_token: remember_token && token.is_some(),
    };
    let target = DesktopServer::remote(profile.clone(), token.clone());
    let target = match session {
        Some(session) => target.with_session_tokens(session)?,
        None => target,
    };
    let client = target.client().await?;
    test_client(&client, "server").await?;

    if remember_token {
        if let Some(token) = token.as_deref() {
            credentials.put(&profile_id, token).await?;
        }
    } else if had_saved_token {
        credentials.delete(&profile_id).await?;
    }
    if let Some(index) = profiles.iter().position(|saved| saved.id == profile_id) {
        profiles[index] = profile;
    } else {
        profiles.push(profile);
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    store.save(profiles)?;
    targets
        .send(target)
        .map_err(|_| "desktop server supervisor stopped".to_string())?;
    Ok(ManagerOutcome::Connected(name))
}

async fn connect_ssh(
    store: &InstanceStore,
    profiles: &mut Vec<SavedServerProfile>,
    targets: &tokio::sync::watch::Sender<DesktopServer>,
    requested_id: Option<String>,
    name: String,
    destination: String,
    state_dir: String,
) -> Result<(ManagerOutcome, tokio::task::JoinHandle<()>), String> {
    let name = name.trim().to_owned();
    let destination = destination.trim().to_owned();
    if name.is_empty() || name.len() > 120 {
        return Err("server name must be between 1 and 120 characters".into());
    }
    if destination.is_empty()
        || destination.starts_with('-')
        || destination.chars().any(char::is_whitespace)
    {
        return Err("SSH destination must be one OpenSSH host or user@host token".into());
    }
    if state_dir.is_empty()
        || state_dir.starts_with('/')
        || state_dir
            .split('/')
            .any(|part| part.is_empty() || part == "..")
        || !state_dir
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
    {
        return Err("SSH state directory must be a safe relative path".into());
    }

    let helper = std::env::current_exe()
        .map_err(|error| format!("locating sift-desktop executable failed: {error}"))?
        .parent()
        .map(|directory| {
            directory.join(if cfg!(windows) {
                "sift-remote.exe"
            } else {
                "sift-remote"
            })
        })
        .ok_or_else(|| "sift-desktop executable has no parent directory".to_string())?;
    if !helper.is_file() {
        return Err(format!(
            "SSH helper is not installed beside sift-desktop: {}",
            helper.display()
        ));
    }

    let mut child = tokio::process::Command::new(&helper)
        .arg(&destination)
        .arg("--state-dir")
        .arg(&state_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("starting SSH helper failed: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "SSH helper stdout was unavailable".to_string())?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let first = match tokio::time::timeout(std::time::Duration::from_secs(120), lines.next_line())
        .await
    {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            return Err(
                ssh_helper_failure(&mut child, "SSH helper exited before remote readiness").await,
            )
        }
        Ok(Err(error)) => {
            return Err(ssh_helper_failure(
                &mut child,
                &format!("reading SSH helper readiness failed: {error}"),
            )
            .await)
        }
        Err(_) => {
            return Err(ssh_helper_failure(
                &mut child,
                "SSH helper timed out before remote readiness",
            )
            .await)
        }
    };
    let ready: sift_protocol::RemoteReady = serde_json::from_str(&first).map_err(|error| {
        format!("decoding SSH helper readiness failed: {error}; output: {first}")
    })?;

    let profile_id = requested_id
        .filter(|id| profiles.iter().any(|profile| profile.id == *id))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let runtime_profile = SavedServerProfile {
        id: profile_id.clone(),
        name: name.clone(),
        base_url: ready.local_base_url.clone(),
        kind: SavedServerKind::Ssh,
        ssh_state_dir: Some(state_dir.clone()),
        has_saved_token: false,
    };
    let target = DesktopServer::remote(runtime_profile, Some(ready.access_token));
    test_client(&target.client().await?, "SSH server").await?;

    let saved = SavedServerProfile {
        id: profile_id.clone(),
        name: name.clone(),
        base_url: destination,
        kind: SavedServerKind::Ssh,
        ssh_state_dir: Some(state_dir.clone()),
        has_saved_token: false,
    };
    if let Some(index) = profiles.iter().position(|profile| profile.id == profile_id) {
        profiles[index] = saved;
    } else {
        profiles.push(saved);
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    store.save(profiles)?;
    targets
        .send(target)
        .map_err(|_| "desktop server supervisor stopped".to_string())?;

    let renewal_targets = targets.clone();
    let renewal_name = name.clone();
    let task = tokio::spawn(async move {
        let stderr_task = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut stderr = tokio::io::BufReader::new(stderr);
                let mut sink = Vec::new();
                let _ = stderr.read_to_end(&mut sink).await;
            })
        });
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(ready) = serde_json::from_str::<sift_protocol::RemoteReady>(&line) else {
                continue;
            };
            let profile = SavedServerProfile {
                id: profile_id.clone(),
                name: renewal_name.clone(),
                base_url: ready.local_base_url,
                kind: SavedServerKind::Ssh,
                ssh_state_dir: Some(state_dir.clone()),
                has_saved_token: false,
            };
            let _ = renewal_targets.send(DesktopServer::remote(profile, Some(ready.access_token)));
        }
        let _ = child.kill().await;
        if let Some(task) = stderr_task {
            task.abort();
        }
    });
    Ok((ManagerOutcome::Connected(name), task))
}

async fn ssh_helper_failure(child: &mut tokio::process::Child, summary: &str) -> String {
    let _ = child.kill().await;
    let mut detail = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut detail).await;
    }
    let detail = detail.trim();
    if detail.is_empty() {
        summary.to_owned()
    } else {
        format!("{summary}: {detail}")
    }
}

async fn test_client(client: &sift_client_sdk::Client, label: &str) -> Result<(), String> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        client
            .connect()
            .await
            .map_err(|error| format!("{label} handshake failed: {error}"))?;
        match client.whoami().await {
            Ok(_) => {}
            Err(sift_client_sdk::Error::Server { status, .. })
                if status == reqwest::StatusCode::UNAUTHORIZED =>
            {
                // The server is valid and reachable. Selecting it must be
                // allowed so the account popover can complete authentication.
            }
            Err(error) => return Err(format!("{label} authentication failed: {error}")),
        }
        Ok(())
    })
    .await
    .map_err(|_| format!("{label} connection test timed out after 10 seconds"))?
}

async fn forget(
    store: &InstanceStore,
    credentials: &DesktopCredentialStore,
    profiles: &mut Vec<SavedServerProfile>,
    profile_id: &str,
) -> Result<ManagerOutcome, String> {
    credentials.delete(profile_id).await?;
    credentials.delete_session(profile_id).await?;
    profiles.retain(|profile| profile.id != profile_id);
    store.save(profiles)?;
    Ok(ManagerOutcome::ProfilesChanged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_store_round_trips_without_secret_material() {
        let directory = tempfile::tempdir().unwrap();
        let store = InstanceStore::new(directory.path().join("instances.json"));
        let profile = SavedServerProfile {
            id: "one".into(),
            name: "LAN".into(),
            base_url: "https://sift.lan".into(),
            kind: SavedServerKind::Hosted,
            ssh_state_dir: None,
            has_saved_token: true,
        };
        store.save(std::slice::from_ref(&profile)).unwrap();
        let bytes = std::fs::read(store.path()).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(!json.contains("token"));
        let loaded = store.load();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].has_saved_token);
    }

    #[test]
    fn profile_and_instance_root_updates_preserve_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let store = InstanceStore::new(directory.path().join("instances.json"));
        let profile = SavedServerProfile {
            id: "one".into(),
            name: "LAN".into(),
            base_url: "https://sift.lan".into(),
            kind: SavedServerKind::Hosted,
            ssh_state_dir: None,
            has_saved_token: false,
        };
        let root = SavedInstanceRoot {
            manifest_id: "manifest-one".into(),
            name: "Local config".into(),
            root: directory.path().join("root"),
        };

        store.save(std::slice::from_ref(&profile)).unwrap();
        store
            .save_roots(std::slice::from_ref(&profile), std::slice::from_ref(&root))
            .unwrap();
        store.save(std::slice::from_ref(&profile)).unwrap();

        assert_eq!(store.load(), vec![profile]);
        assert_eq!(store.load_roots(), vec![root]);
    }

    #[test]
    fn forgetting_a_missing_instance_root_removes_only_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let store = InstanceStore::new(directory.path().join("instances.json"));
        let stale = SavedInstanceRoot {
            manifest_id: "missing".into(),
            name: "Missing root".into(),
            root: directory.path().join("already-deleted"),
        };
        let mut roots = vec![stale.clone()];
        store.save_roots(&[], &roots).unwrap();
        let target = DesktopServer::remote(
            SavedServerProfile {
                id: "remote".into(),
                name: "Remote".into(),
                base_url: "https://sift.invalid".into(),
                kind: SavedServerKind::Hosted,
                ssh_state_dir: None,
                has_saved_token: false,
            },
            None,
        );
        let (targets, _) = tokio::sync::watch::channel(target.clone());
        let mut configured = std::collections::HashMap::new();

        assert!(matches!(
            forget_root(
                &store,
                &[],
                &mut roots,
                &mut configured,
                &targets,
                &target,
                &stale.root,
            ),
            Ok(ManagerOutcome::None)
        ));
        assert!(roots.is_empty());
        assert!(store.load_roots().is_empty());
    }
}
