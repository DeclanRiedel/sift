//! Pure-data authentication wire contract. Secret-bearing structs implement
//! redacted `Debug` manually so tracing an extractor cannot expose them.

use std::fmt;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RedactedString(pub String);

impl fmt::Debug for RedactedString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

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

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

impl fmt::Debug for ChangePasswordRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangePasswordRequest")
            .field("current_password", &"[REDACTED]")
            .field("new_password", &"[REDACTED]")
            .finish()
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateGithubAllowlistRequest {
    pub login: String,
    /// Explicitly links the first successful OAuth callback to an existing
    /// principal. `None` creates a new principal and personal tenant.
    pub target_principal_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationRole {
    Admin,
    Member,
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateTenantInvitationRequest {
    pub role: InvitationRole,
    pub target_principal_id: Option<i64>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcceptTenantInvitationRequest {
    pub token: String,
}

impl fmt::Debug for AcceptTenantInvitationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptTenantInvitationRequest")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct IssuedTenantInvitationResponse {
    pub invitation_id: i64,
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RegisterPrincipalKeyRequest {
    /// Base64url-no-pad encoded 32-byte Ed25519 public key.
    pub public_key: String,
    pub label: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdminCreatePasswordPrincipalRequest {
    pub username: String,
    pub password: String,
    pub display_name: String,
    pub email: Option<String>,
    #[serde(default)]
    pub is_instance_admin: bool,
}

impl fmt::Debug for AdminCreatePasswordPrincipalRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminCreatePasswordPrincipalRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("display_name", &self.display_name)
            .field("email", &self.email)
            .field("is_instance_admin", &self.is_instance_admin)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdminSetPrincipalDisabledRequest {
    pub disabled: bool,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdminLinkPasswordIdentityRequest {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for AdminLinkPasswordIdentityRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminLinkPasswordIdentityRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthIdentitySummary {
    pub id: i64,
    pub method: String,
    pub issuer: String,
    pub subject: String,
    pub provider_login: Option<String>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthSessionSummary {
    pub id: String,
    pub client_kind: String,
    pub client_label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revocation_reason: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct PasswordResetRequest {
    pub token: String,
    pub new_password: String,
}

impl fmt::Debug for PasswordResetRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PasswordResetRequest")
            .field("token", &"[REDACTED]")
            .field("new_password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct IssuedPasswordResetResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for IssuedPasswordResetResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedPasswordResetResponse")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeyChallengeRequest {
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeyChallengeResponse {
    pub nonce: String,
    pub message: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeyAuthenticateRequest {
    pub nonce: String,
    pub signature: String,
}

impl fmt::Debug for KeyAuthenticateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KeyAuthenticateRequest")
            .field("nonce", &"[REDACTED]")
            .field("signature", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for IssuedTenantInvitationResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedTenantInvitationResponse")
            .field("invitation_id", &self.invitation_id)
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
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
        let change = ChangePasswordRequest {
            current_password: "current secret".into(),
            new_password: "new secret".into(),
        };
        let debug = format!("{login:?} {refresh:?} {tokens:?} {change:?}");
        assert!(!debug.contains("correct horse"));
        assert!(!debug.contains("sift_rt_secret"));
        assert!(!debug.contains("sift_at_secret"));
        assert!(!debug.contains("current secret"));
        assert!(!debug.contains("new secret"));
    }
}
