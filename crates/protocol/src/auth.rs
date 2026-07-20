//! Pure-data authentication wire contract. Secret-bearing structs implement
//! redacted `Debug` manually so tracing an extractor cannot expose them.

use std::fmt;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthClientKind {
    #[default]
    Native,
    Web,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct PasswordLoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub client_kind: AuthClientKind,
    #[serde(default)]
    pub client_label: Option<String>,
}

impl fmt::Debug for PasswordLoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PasswordLoginRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("client_kind", &self.client_kind)
            .field("client_label", &self.client_label)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct RefreshAuthRequest {
    #[serde(default)]
    pub refresh_token: Option<String>,
}

impl fmt::Debug for RefreshAuthRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshAuthRequest")
            .field("refresh_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthTokensResponse {
    pub access_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebAuthResponse {
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
    /// Double-submit value. This is not an authentication credential; browser
    /// clients echo it in `X-Sift-CSRF` for state-changing requests.
    pub csrf_token: String,
}

impl fmt::Debug for WebAuthResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebAuthResponse")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("csrf_token", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for AuthTokensResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthTokensResponse")
            .field("access_token", &"[REDACTED]")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_token", &"[REDACTED]")
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthPrincipal {
    pub id: i64,
    pub display_name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub is_instance_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthTenantMembership {
    pub tenant_id: i64,
    pub tenant_name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WhoAmIResponse {
    pub principal: AuthPrincipal,
    pub memberships: Vec<AuthTenantMembership>,
    /// Present for interactive sessions; absent for API tokens and local
    /// bypass identities.
    pub auth_session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_bearing_debug_is_redacted() {
        let login = PasswordLoginRequest {
            username: "alice".into(),
            password: "correct horse battery staple".into(),
            client_kind: AuthClientKind::Native,
            client_label: None,
        };
        let refresh = RefreshAuthRequest {
            refresh_token: Some("sift_rt_secret".into()),
        };
        let tokens = AuthTokensResponse {
            access_token: "sift_at_secret".into(),
            access_expires_at: Utc::now(),
            refresh_token: "sift_rt_secret".into(),
            refresh_expires_at: Utc::now(),
        };
        let debug = format!("{login:?} {refresh:?} {tokens:?}");
        assert!(!debug.contains("correct horse"));
        assert!(!debug.contains("sift_rt_secret"));
        assert!(!debug.contains("sift_at_secret"));
    }
}
