use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use keyring::{Entry, Error as KeyringError};
use sift_client_sdk::SessionTokenProvider;
use sift_protocol::{AuthClientKind, AuthTokensResponse, PasswordLoginRequest};
use sift_workspace_ui::{InstanceCommand, InstanceManagerEvent, SavedServerProfile};

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
}

impl InstanceStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn load(&self) -> Vec<SavedServerProfile> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StoredInstances>(&bytes).ok())
            .filter(|stored| stored.version == PROFILE_VERSION)
            .map(|mut stored| {
                for profile in &mut stored.profiles {
                    profile.has_saved_token = false;
                }
                stored.profiles
            })
            .unwrap_or_default()
    }

    pub fn save(&self, profiles: &[SavedServerProfile]) -> Result<(), String> {
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
    mut channels: InstanceManagerChannels,
    local_target: DesktopServer,
    restored_profile_id: Option<String>,
) {
    annotate_saved_tokens(&mut profiles, &credentials).await;
    if let Some(profile) = restored_profile_id
        .as_deref()
        .and_then(|id| profiles.iter().find(|profile| profile.id == id))
        .cloned()
    {
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
    if channels
        .events
        .send(InstanceManagerEvent::Profiles(profiles.clone()))
        .is_err()
    {
        return;
    }
    while let Some(command) = channels.commands.recv().await {
        let (authentication, result) = match command {
            InstanceCommand::UseLocal => {
                (false, connect_local(&channels.targets, &local_target).await)
            }
            InstanceCommand::Connect {
                profile_id,
                name,
                base_url,
                bearer_token,
                remember_token,
            } => {
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
            InstanceCommand::Forget { profile_id } => (
                false,
                forget(&store, &credentials, &mut profiles, &profile_id).await,
            ),
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
    let token = match bearer_token {
        Some(token) => Some(validate_token(token)?),
        None if had_saved_token => credentials.get(&profile_id).await?,
        None => None,
    };
    let profile = SavedServerProfile {
        id: profile_id.clone(),
        name: name.clone(),
        base_url,
        has_saved_token: remember_token && token.is_some(),
    };
    let target = DesktopServer::remote(profile.clone(), token.clone());
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
}
