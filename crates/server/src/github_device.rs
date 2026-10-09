//! Local owner device authorization. Provider credentials never enter SQLite.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sift_metadata::{GithubProfile, PrincipalId};
use sift_protocol::{GithubOwnerDevicePollResponse, GithubOwnerDeviceStartResponse};

const MAX_ATTEMPTS: usize = 32;
const MAX_LIFETIME: u64 = 900;

#[derive(Clone)]
pub struct GithubOwnerDevice {
    client_id: String,
    http: reqwest::Client,
    endpoints: Endpoints,
    attempts: Arc<Mutex<HashMap<[u8; 32], AttemptEntry>>>,
}

#[derive(Clone)]
struct Endpoints {
    device: String,
    token: String,
    profile: String,
}

#[derive(Clone)]
struct AttemptEntry {
    owner: PrincipalId,
    expires: Instant,
    state: Arc<tokio::sync::Mutex<Attempt>>,
}

struct Attempt {
    expires: Instant,
    next_poll: Instant,
    interval: u64,
    device_code: Option<String>,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct ProfileResponse {
    id: u64,
    login: String,
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

pub enum DevicePoll {
    Pending(GithubOwnerDevicePollResponse),
    Authorized(GithubProfile),
}

impl GithubOwnerDevice {
    pub fn new(client_id: String) -> anyhow::Result<Self> {
        Ok(Self {
            client_id,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoints: Endpoints {
                device: "https://github.com/login/device/code".into(),
                token: "https://github.com/login/oauth/access_token".into(),
                profile: "https://api.github.com/user".into(),
            },
            attempts: Default::default(),
        })
    }

    pub async fn start(
        &self,
        owner: PrincipalId,
    ) -> Result<GithubOwnerDeviceStartResponse, &'static str> {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let key = handoff_key(&token);
        let now = Instant::now();
        let attempt = Arc::new(tokio::sync::Mutex::new(Attempt {
            expires: now + Duration::from_secs(MAX_LIFETIME),
            next_poll: now,
            interval: 5,
            device_code: None,
        }));
        {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| "GitHub owner setup unavailable")?;
            attempts.retain(|_, attempt| attempt.expires > now && attempt.owner != owner);
            if attempts.len() >= MAX_ATTEMPTS {
                return Err("Too many GitHub setup attempts; try again later");
            }
            attempts.insert(
                key,
                AttemptEntry {
                    owner,
                    expires: now + Duration::from_secs(MAX_LIFETIME),
                    state: attempt.clone(),
                },
            );
        }
        let result = self.start_upstream(&token, &attempt).await;
        if result.is_err() {
            self.remove(&key);
        }
        result
    }

    async fn start_upstream(
        &self,
        token: &str,
        attempt: &tokio::sync::Mutex<Attempt>,
    ) -> Result<GithubOwnerDeviceStartResponse, &'static str> {
        let response = self
            .http
            .post(&self.endpoints.device)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", "read:user"),
            ])
            .send()
            .await
            .map_err(|_| "Cannot reach GitHub; retry owner setup")?
            .error_for_status()
            .map_err(|_| {
                "GitHub rejected setup; check the OAuth App client ID and enable device flow"
            })?;
        let device: DeviceResponse = response
            .json()
            .await
            .map_err(|_| "Check the OAuth App client ID and enable device flow")?;
        if device.verification_uri != "https://github.com/login/device"
            || device.device_code.is_empty()
            || device.device_code.len() > 256
            || device.user_code.is_empty()
            || device.user_code.len() > 32
            || device.expires_in == 0
            || device.expires_in > MAX_LIFETIME
            || device.interval == 0
            || device.interval > MAX_LIFETIME
        {
            return Err("GitHub returned invalid device authorization");
        }
        let mut state = attempt.lock().await;
        state.interval = device.interval;
        state.expires = Instant::now() + Duration::from_secs(device.expires_in);
        state.next_poll = Instant::now() + Duration::from_secs(device.interval);
        state.device_code = Some(device.device_code);
        Ok(GithubOwnerDeviceStartResponse {
            verification_uri: device.verification_uri,
            user_code: device.user_code,
            handoff_token: token.into(),
            expires_in: device.expires_in,
            interval_secs: device.interval,
        })
    }

    pub async fn poll(&self, token: &str, owner: PrincipalId) -> Result<DevicePoll, &'static str> {
        if token.len() != 64 {
            return Err("Invalid GitHub setup attempt");
        }
        let key = handoff_key(token);
        let attempt = self
            .attempts
            .lock()
            .map_err(|_| "GitHub owner setup unavailable")?
            .get(&key)
            .cloned()
            .ok_or("GitHub setup expired or was cancelled; start again")?;
        if attempt.owner != owner {
            return Err("Invalid GitHub setup owner");
        }
        let mut attempt = attempt
            .state
            .try_lock()
            .map_err(|_| "GitHub setup is already being checked")?;
        if attempt.expires <= Instant::now() {
            self.remove(&key);
            return Err("GitHub setup expired; start again");
        }
        if attempt.next_poll > Instant::now() {
            return Ok(DevicePoll::Pending(pending(attempt.interval)));
        }
        let code = attempt
            .device_code
            .as_deref()
            .ok_or("GitHub setup is not ready")?;
        let token_response = self
            .http
            .post(&self.endpoints.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("device_code", code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await;
        // Network failures also consume the polling interval.
        attempt.next_poll = Instant::now() + Duration::from_secs(attempt.interval);
        let response: TokenResponse = token_response
            .map_err(|_| "Cannot reach GitHub; retry owner setup")?
            .error_for_status()
            .map_err(|_| "GitHub authorization failed; start again")?
            .json()
            .await
            .map_err(|_| "GitHub authorization failed; start again")?;
        match response.error.as_deref() {
            Some("authorization_pending") => {
                return Ok(DevicePoll::Pending(pending(attempt.interval)))
            }
            Some("slow_down") => {
                attempt.interval = response
                    .interval
                    .unwrap_or(attempt.interval.saturating_add(5))
                    .max(attempt.interval.saturating_add(5))
                    .min(MAX_LIFETIME);
                attempt.next_poll = Instant::now() + Duration::from_secs(attempt.interval);
                return Ok(DevicePoll::Pending(pending(attempt.interval)));
            }
            Some(_) => {
                self.remove(&key);
                return Err("GitHub authorization denied or expired; start again");
            }
            None => {}
        }
        let token = response
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or("GitHub did not authorize this setup attempt")?;
        let response = self
            .http
            .get(&self.endpoints.profile)
            .bearer_auth(&token)
            .header(reqwest::header::USER_AGENT, "sift")
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await;
        drop(token);
        // Completion is one-use even if the profile fetch or binding fails.
        let active = self.remove(&key);
        if !active || attempt.expires <= Instant::now() {
            return Err("GitHub setup expired or was cancelled; start again");
        }
        let profile: ProfileResponse = response
            .map_err(|_| "Cannot load GitHub account; start again")?
            .error_for_status()
            .map_err(|_| "Cannot load GitHub account; start again")?
            .json()
            .await
            .map_err(|_| "Invalid GitHub account response")?;
        Ok(DevicePoll::Authorized(GithubProfile {
            id: profile.id,
            login: profile.login,
            display_name: profile.name,
            email: profile.email,
            avatar_url: profile.avatar_url,
        }))
    }

    pub fn cancel(&self, token: &str, owner: PrincipalId) -> Result<(), &'static str> {
        let key = handoff_key(token);
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| "GitHub owner setup unavailable")?;
        if attempts
            .get(&key)
            .is_some_and(|attempt| attempt.owner != owner)
        {
            return Err("Invalid GitHub setup owner");
        }
        attempts.remove(&key);
        Ok(())
    }

    fn remove(&self, key: &[u8; 32]) -> bool {
        self.attempts
            .lock()
            .map(|mut attempts| attempts.remove(key).is_some())
            .unwrap_or(false)
    }
}

fn handoff_key(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn pending(interval_secs: u64) -> GithubOwnerDevicePollResponse {
    GithubOwnerDevicePollResponse {
        completed: false,
        interval_secs,
        github_login: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn ready(device: &GithubOwnerDevice, token: &str) {
        let attempt = device
            .attempts
            .lock()
            .unwrap()
            .get(&handoff_key(token))
            .unwrap()
            .state
            .clone();
        attempt.lock().await.next_poll = Instant::now();
    }

    #[tokio::test]
    async fn device_authorization_honors_interval_owner_expiry_and_single_completion() {
        let polls = Arc::new(AtomicUsize::new(0));
        let calls = polls.clone();
        let router = Router::new()
            .route("/device", post(|| async { Json(serde_json::json!({
                "device_code": "fixture-device-secret", "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device", "expires_in": 900, "interval": 5,
            })) }))
            .route("/token", post(move || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { Json(match n {
                    0 => serde_json::json!({"error": "authorization_pending"}),
                    1 => serde_json::json!({"error": "slow_down", "interval": 10}),
                    2 => serde_json::json!({"access_token": "fixture-access-secret"}),
                    _ => serde_json::json!({"error": "access_denied"}),
                }) }
            }))
            .route("/profile", get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers[reqwest::header::AUTHORIZATION], "Bearer fixture-access-secret");
                Json(serde_json::json!({"id": 123, "login": "fixture-owner", "name": "Owner"}))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut device = GithubOwnerDevice::new("fixture-client".into()).unwrap();
        device.endpoints = Endpoints {
            device: format!("{base}/device"),
            token: format!("{base}/token"),
            profile: format!("{base}/profile"),
        };
        let start = device.start(PrincipalId(1)).await.unwrap();
        assert!(!format!("{start:?}").contains("ABCD-EFGH"));
        assert!(!format!("{start:?}").contains(&start.handoff_token));
        assert!(device
            .poll(&start.handoff_token, PrincipalId(2))
            .await
            .is_err());
        assert!(matches!(
            device
                .poll(&start.handoff_token, PrincipalId(1))
                .await
                .unwrap(),
            DevicePoll::Pending(_)
        ));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        ready(&device, &start.handoff_token).await;
        assert!(matches!(
            device
                .poll(&start.handoff_token, PrincipalId(1))
                .await
                .unwrap(),
            DevicePoll::Pending(_)
        ));
        ready(&device, &start.handoff_token).await;
        let DevicePoll::Pending(slow) = device
            .poll(&start.handoff_token, PrincipalId(1))
            .await
            .unwrap()
        else {
            panic!("expected slow-down");
        };
        assert_eq!(slow.interval_secs, 10);
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        assert!(matches!(
            device
                .poll(&start.handoff_token, PrincipalId(1))
                .await
                .unwrap(),
            DevicePoll::Pending(_)
        ));
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        ready(&device, &start.handoff_token).await;
        let DevicePoll::Authorized(profile) = device
            .poll(&start.handoff_token, PrincipalId(1))
            .await
            .unwrap()
        else {
            panic!("expected profile");
        };
        assert_eq!(profile.id, 123);
        assert!(device
            .poll(&start.handoff_token, PrincipalId(1))
            .await
            .is_err());
        let denied = device.start(PrincipalId(1)).await.unwrap();
        ready(&device, &denied.handoff_token).await;
        assert!(device
            .poll(&denied.handoff_token, PrincipalId(1))
            .await
            .is_err());
        assert!(!device
            .attempts
            .lock()
            .unwrap()
            .contains_key(&handoff_key(&denied.handoff_token)));
        let first = device.start(PrincipalId(1)).await.unwrap();
        let second = device.start(PrincipalId(1)).await.unwrap();
        assert!(device
            .poll(&first.handoff_token, PrincipalId(1))
            .await
            .is_err());
        assert!(device
            .cancel(&second.handoff_token, PrincipalId(2))
            .is_err());
        device
            .cancel(&second.handoff_token, PrincipalId(1))
            .unwrap();
        assert!(device
            .poll(&second.handoff_token, PrincipalId(1))
            .await
            .is_err());
        let expired = device.start(PrincipalId(1)).await.unwrap();
        let attempt = device
            .attempts
            .lock()
            .unwrap()
            .get(&handoff_key(&expired.handoff_token))
            .unwrap()
            .state
            .clone();
        attempt.lock().await.expires = Instant::now();
        assert!(device
            .poll(&expired.handoff_token, PrincipalId(1))
            .await
            .is_err());
        server.abort();
    }
}
