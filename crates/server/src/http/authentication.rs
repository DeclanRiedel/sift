//! Authentication, identity and invitation handlers.

use super::*;

pub(super) async fn password_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PasswordLoginRequest>,
) -> ApiResult<Response> {
    let client_kind = request.client_kind;
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let metadata = metadata_store(&state)?;
    let outcome = state
        .auth
        .runtime
        .authenticate_password(
            metadata,
            source,
            &request.username,
            request.password.into_bytes(),
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let identity = match outcome {
        crate::identity::PasswordAuthOutcome::Authenticated(identity) => identity,
        crate::identity::PasswordAuthOutcome::Denied => {
            record_auth_failure(metadata, "authenticate.password", "denied")?;
            state.sessions.push_operation_full(
                Operation::Authenticate {
                    method: sift_protocol::AuthenticationMethod::Password,
                },
                OperationStatus::Failed,
                None,
                Some("authentication_denied".into()),
                None,
                Some("authentication denied".into()),
            );
            return Err(ApiError::Unauthorized);
        }
        crate::identity::PasswordAuthOutcome::Throttled => {
            record_auth_failure(metadata, "authenticate.password", "throttled")?;
            return Err(ApiError::TooManyAuthAttempts);
        }
    };
    let tokens = metadata
        .issue_auth_session(
            identity.principal.id,
            match client_kind {
                AuthClientKind::Native => MetadataAuthClientKind::Native,
                AuthClientKind::Web => MetadataAuthClientKind::Web,
            },
            request.client_label.as_deref(),
            NewOperationAudit {
                actor_principal_id: Some(identity.principal.id),
                action: "authenticate.password".into(),
                target: "auth_session".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Password,
        },
        OperationStatus::Succeeded,
        Some(identity.principal.id.0),
        None,
        None,
        None,
    );
    auth_login_response(tokens, client_kind == AuthClientKind::Web)
}

pub(super) async fn refresh_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RefreshAuthRequest>,
) -> ApiResult<Response> {
    let metadata = metadata_store(&state)?;
    let cookie_refresh = cookie_value(&headers, "sift_refresh");
    if cookie_refresh.is_some() && !valid_csrf(&headers) {
        return Err(ApiError::Forbidden("invalid CSRF token".into()));
    }
    let presented = request
        .refresh_token
        .as_deref()
        .or(cookie_refresh)
        .ok_or(ApiError::Unauthorized)?;
    let audit = NewOperationAudit {
        actor_principal_id: None,
        action: "refresh_auth_session".into(),
        target: "auth_session".into(),
        target_id: None,
        status: "succeeded".into(),
        result_code: None,
        row_count: None,
        error_message: None,
        correlation_id: crate::correlation::current(),
    };
    match metadata.rotate_auth_refresh_token(presented, audit).await? {
        RefreshAuthResult::Issued(tokens) => {
            state
                .auth
                .runtime
                .invalidate_auth_session(&tokens.session_id);
            state.sessions.push_operation_local(
                Operation::RefreshAuthSession,
                OperationStatus::Succeeded,
                None,
                None,
                None,
                None,
            );
            auth_login_response(tokens, cookie_refresh.is_some())
        }
        RefreshAuthResult::ReplayDetected => {
            state.auth.runtime.invalidate_all_access_tokens();
            Err(ApiError::Unauthorized)
        }
        RefreshAuthResult::Invalid => Err(ApiError::Unauthorized),
    }
}

pub(super) async fn logout_auth(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Response> {
    let session_id = auth.auth_session_id.as_deref().ok_or_else(|| {
        ApiError::BadRequest("the current credential is not an interactive session".into())
    })?;
    metadata_store(&state)?.revoke_auth_session(
        session_id,
        "logout",
        metadata_audit_record(auth.principal_id, "logout", "auth_session", None),
    )?;
    state.auth.runtime.invalidate_auth_session(session_id);
    state
        .sessions
        .disconnect_managed_principal(auth.principal_id)
        .await;
    state.sessions.push_operation_local(
        Operation::Logout {
            all_sessions: false,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(logout_response(auth.cookie_authenticated))
}

pub(super) async fn logout_all_auth(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Response> {
    metadata_store(&state)?.revoke_all_auth_sessions(
        auth.principal_id,
        "logout_all",
        metadata_audit_record(auth.principal_id, "logout_all", "auth_session", None),
    )?;
    state.auth.runtime.invalidate_principal(auth.principal_id);
    state
        .sessions
        .disconnect_managed_principal(auth.principal_id)
        .await;
    state.sessions.push_operation_local(
        Operation::Logout { all_sessions: true },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(logout_response(auth.cookie_authenticated))
}

pub(super) async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<ChangePasswordRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let identity = metadata
        .list_auth_identities(auth.principal_id)?
        .into_iter()
        .find(|identity| {
            identity.method == sift_metadata::AuthIdentityMethod::Password
                && identity.disabled_at.is_none()
        })
        .ok_or_else(|| ApiError::BadRequest("principal has no password identity".into()))?;
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let verified = state
        .auth
        .runtime
        .authenticate_password(
            metadata,
            source,
            &identity.subject,
            request.current_password.into_bytes(),
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    match verified {
        crate::identity::PasswordAuthOutcome::Authenticated(password)
            if password.principal.id == auth.principal_id => {}
        crate::identity::PasswordAuthOutcome::Throttled => {
            return Err(ApiError::TooManyAuthAttempts)
        }
        crate::identity::PasswordAuthOutcome::Authenticated(_)
        | crate::identity::PasswordAuthOutcome::Denied => return Err(ApiError::Unauthorized),
    }
    let verifier = crate::identity::hash_password(request.new_password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    metadata
        .replace_password_verifier(
            identity.id,
            verifier.as_bytes(),
            metadata_audit_record(
                auth.principal_id,
                "change_password",
                "auth_identity",
                Some(identity.id.0),
            ),
        )
        .await?;
    state.auth.runtime.invalidate_principal(auth.principal_id);
    state
        .sessions
        .disconnect_managed_principal(auth.principal_id)
        .await;
    state.sessions.push_operation_local(
        Operation::ChangePassword,
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn reset_password(
    State(state): State<AppState>,
    Json(request): Json<PasswordResetRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let verifier = state
        .auth
        .runtime
        .hash_password_bounded(request.new_password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?
        .ok_or(ApiError::TooManyAuthAttempts)?;
    let principal = match metadata_store(&state)?
        .consume_password_reset(
            &request.token,
            verifier.as_bytes(),
            NewOperationAudit {
                actor_principal_id: None,
                action: "manage_principal.reset_password".into(),
                target: "auth_identity".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )
        .await
    {
        Ok(principal) => principal,
        Err(sift_metadata::MetadataError::InvalidPasswordReset) => {
            return Err(ApiError::Unauthorized)
        }
        Err(error) => return Err(error.into()),
    };
    state.auth.runtime.invalidate_principal(principal);
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Reset,
            principal_id: Some(principal.0),
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct GithubStartQuery {
    client_kind: Option<AuthClientKind>,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct GithubCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GithubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
pub(super) struct GithubUserResponse {
    id: u64,
    login: String,
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

pub(super) async fn github_start(
    State(state): State<AppState>,
    Query(query): Query<GithubStartQuery>,
) -> ApiResult<Response> {
    use base64::Engine as _;
    use sha2::Digest as _;

    let config =
        state.auth.github.as_ref().ok_or_else(|| {
            ApiError::BadRequest("GitHub authentication is not configured".into())
        })?;
    let client_kind = query.client_kind.unwrap_or(AuthClientKind::Web);
    let attempt = metadata_store(&state)?
        .create_github_oauth_attempt(match client_kind {
            AuthClientKind::Native => MetadataAuthClientKind::Native,
            AuthClientKind::Web => MetadataAuthClientKind::Web,
        })
        .await?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(attempt.code_verifier.as_bytes()));
    let callback = format!(
        "{}/v1/auth/github/callback",
        config.public_base_url.trim_end_matches('/')
    );
    let mut authorize = reqwest::Url::parse("https://github.com/login/oauth/authorize")
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    authorize.query_pairs_mut().extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", callback.as_str()),
        ("scope", "read:user"),
        ("state", attempt.state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("allow_signup", "false"),
    ]);
    if client_kind == AuthClientKind::Native {
        Ok(Json(GithubNativeAuthStartResponse {
            authorization_url: authorize.to_string(),
            handoff_token: attempt
                .handoff_token
                .ok_or_else(|| ApiError::Internal("native OAuth handoff missing".into()))?,
        })
        .into_response())
    } else {
        Ok(Redirect::temporary(authorize.as_str()).into_response())
    }
}

pub(super) async fn github_callback(
    State(state): State<AppState>,
    Query(query): Query<GithubCallbackQuery>,
) -> ApiResult<Response> {
    if query.error.is_some() {
        return Err(ApiError::Unauthorized);
    }
    let code = query.code.as_deref().ok_or(ApiError::Unauthorized)?;
    let oauth_state = query.state.as_deref().ok_or(ApiError::Unauthorized)?;
    let config =
        state.auth.github.as_ref().ok_or_else(|| {
            ApiError::BadRequest("GitHub authentication is not configured".into())
        })?;
    let attempt = metadata_store(&state)?
        .consume_github_oauth_attempt(oauth_state)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    let callback = format!(
        "{}/v1/auth/github/callback",
        config.public_base_url.trim_end_matches('/')
    );
    let token_response = config
        .http
        .post("https://github.com/login/oauth/access_token")
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", callback.as_str()),
            ("code_verifier", attempt.code_verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    if !token_response.status().is_success() {
        return Err(ApiError::Unauthorized);
    }
    let token: GithubTokenResponse = token_response
        .json()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    let user_response = config
        .http
        .get("https://api.github.com/user")
        .bearer_auth(&token.access_token)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header(header::USER_AGENT, "sift")
        .send()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    if !user_response.status().is_success() {
        return Err(ApiError::Unauthorized);
    }
    let user: GithubUserResponse = user_response
        .json()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    // `token` is dropped immediately after this profile fetch and is never
    // persisted or included in operation/audit values.
    drop(token);
    let metadata = metadata_store(&state)?;
    let principal = metadata
        .complete_github_identity(
            GithubProfile {
                id: user.id,
                login: user.login,
                display_name: user.name,
                email: user.email,
                avatar_url: user.avatar_url,
            },
            NewOperationAudit {
                actor_principal_id: None,
                action: "authenticate.github".into(),
                target: "auth_identity".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )?
        .ok_or(ApiError::Unauthorized)?;
    if attempt.client_kind == MetadataAuthClientKind::Native {
        metadata.complete_native_oauth_attempt(&attempt.attempt_id, principal.id)?;
        return Ok(Json(json!({
            "ok": true,
            "message": "GitHub authentication complete; return to Sift"
        }))
        .into_response());
    }
    let tokens = metadata
        .issue_auth_session(
            principal.id,
            MetadataAuthClientKind::Web,
            Some("GitHub OAuth"),
            metadata_audit_record(
                principal.id,
                "authenticate.github.session",
                "auth_session",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Github,
        },
        OperationStatus::Succeeded,
        Some(principal.id.0),
        None,
        None,
        None,
    );
    auth_login_response(tokens, true)
}

pub(super) async fn github_native_exchange(
    State(state): State<AppState>,
    Json(request): Json<GithubNativeAuthExchangeRequest>,
) -> ApiResult<Json<AuthTokensResponse>> {
    let metadata = metadata_store(&state)?;
    let principal = metadata
        .consume_native_oauth_handoff(&request.handoff_token)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    let tokens = metadata
        .issue_auth_session(
            principal,
            MetadataAuthClientKind::Native,
            Some("GitHub OAuth native handoff"),
            metadata_audit_record(
                principal,
                "authenticate.github.session",
                "auth_session",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Github,
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
    Ok(Json(AuthTokensResponse {
        access_token: tokens.access_token,
        access_expires_at: tokens.access_expires_at,
        refresh_token: tokens.refresh_token,
        refresh_expires_at: tokens.refresh_expires_at,
    }))
}

pub(super) async fn create_github_allowlist(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<CreateGithubAllowlistRequest>,
) -> ApiResult<Json<sift_metadata::GithubAllowlistEntry>> {
    ensure_instance_admin(&state, &auth)?;
    let login = crate::identity::normalize_github_login(&request.login)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let target = request.target_principal_id.map(PrincipalId);
    if let Some(target) = target {
        metadata_store(&state)?
            .principal_by_id(target)?
            .ok_or(ApiError::Metadata(
                sift_metadata::MetadataError::PrincipalNotFound(target),
            ))?;
    }
    let entry = metadata_store(&state)?.create_github_allowlist_entry(
        &login,
        target,
        auth.principal_id,
        metadata_audit_record(
            auth.principal_id,
            "github_allowlist.create",
            "github_allowlist",
            None,
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageGithubAllowlist {
            action: sift_protocol::IdentityAdminAction::Create,
            principal_id: request.target_principal_id,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(entry))
}

pub(super) async fn list_github_allowlist(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<Vec<sift_metadata::GithubAllowlistEntry>>> {
    ensure_instance_admin(&state, &auth)?;
    Ok(Json(
        metadata_store(&state)?.list_github_allowlist_entries()?,
    ))
}

pub(super) async fn revoke_github_allowlist(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?.revoke_github_allowlist_entry(
        GithubAllowlistId(id),
        metadata_audit_record(
            auth.principal_id,
            "github_allowlist.revoke",
            "github_allowlist",
            Some(id),
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageGithubAllowlist {
            action: sift_protocol::IdentityAdminAction::Revoke,
            principal_id: None,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn admin_create_principal(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<AdminCreatePasswordPrincipalRequest>,
) -> ApiResult<Json<AuthPrincipal>> {
    ensure_instance_admin(&state, &auth)?;
    let username = crate::identity::normalize_username(&request.username)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    if request.display_name.trim().is_empty() || request.display_name.len() > 200 {
        return Err(ApiError::BadRequest(
            "display name must be between 1 and 200 characters".into(),
        ));
    }
    let verifier = crate::identity::hash_password(request.password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let principal = metadata_store(&state)?
        .create_password_principal(
            sift_metadata::NewPasswordPrincipal {
                username: &username,
                display_name: request.display_name.trim(),
                email: request.email.as_deref(),
                is_instance_admin: request.is_instance_admin,
            },
            verifier.as_bytes(),
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.create",
                "principal",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Create,
            principal_id: Some(principal.id.0),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(AuthPrincipal {
        id: principal.id.0,
        display_name: principal.display_name,
        email: principal.email,
        avatar_url: principal.avatar_url,
        is_instance_admin: principal.is_instance_admin,
    }))
}

pub(super) async fn admin_set_principal_disabled(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
    Json(request): Json<AdminSetPrincipalDisabledRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?.set_principal_disabled(
        PrincipalId(id),
        request.disabled,
        metadata_audit_record(
            auth.principal_id,
            if request.disabled {
                "manage_principal.disable"
            } else {
                "manage_principal.enable"
            },
            "principal",
            Some(id),
        ),
    )?;
    state.auth.runtime.invalidate_principal(PrincipalId(id));
    if request.disabled {
        state
            .sessions
            .disconnect_managed_principal(PrincipalId(id))
            .await;
    }
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: if request.disabled {
                sift_protocol::IdentityAdminAction::Disable
            } else {
                sift_protocol::IdentityAdminAction::Enable
            },
            principal_id: Some(id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn admin_list_principal_identities(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<AuthIdentitySummary>>> {
    ensure_instance_admin(&state, &auth)?;
    if metadata_store(&state)?
        .principal_by_id(PrincipalId(id))?
        .is_none()
    {
        return Err(ApiError::Metadata(
            sift_metadata::MetadataError::PrincipalNotFound(PrincipalId(id)),
        ));
    }
    let identities = metadata_store(&state)?
        .list_auth_identities(PrincipalId(id))?
        .into_iter()
        .map(|identity| AuthIdentitySummary {
            id: identity.id.0,
            method: format!("{:?}", identity.method).to_lowercase(),
            issuer: identity.issuer,
            subject: identity.subject,
            provider_login: identity.provider_login,
            disabled: identity.disabled_at.is_some(),
        })
        .collect();
    Ok(Json(identities))
}

pub(super) async fn admin_link_password_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
    Json(request): Json<AdminLinkPasswordIdentityRequest>,
) -> ApiResult<Json<AuthIdentitySummary>> {
    ensure_instance_admin(&state, &auth)?;
    let username = crate::identity::normalize_username(&request.username)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let verifier = crate::identity::hash_password(request.password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let identity = metadata_store(&state)?
        .link_password_identity(
            PrincipalId(id),
            &username,
            verifier.as_bytes(),
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.link",
                "auth_identity",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Link,
            principal_id: Some(id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(AuthIdentitySummary {
        id: identity.id.0,
        method: "password".into(),
        issuer: identity.issuer,
        subject: identity.subject,
        provider_login: identity.provider_login,
        disabled: false,
    }))
}

pub(super) async fn admin_unlink_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((principal_id, identity_id)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?
        .unlink_auth_identity(
            PrincipalId(principal_id),
            AuthIdentityId(identity_id),
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.unlink",
                "auth_identity",
                Some(identity_id),
            ),
        )
        .await?;
    state
        .auth
        .runtime
        .invalidate_principal(PrincipalId(principal_id));
    state
        .sessions
        .disconnect_managed_principal(PrincipalId(principal_id))
        .await;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Unlink,
            principal_id: Some(principal_id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn admin_list_auth_sessions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<AuthSessionSummary>>> {
    ensure_instance_admin(&state, &auth)?;
    if metadata_store(&state)?
        .principal_by_id(PrincipalId(id))?
        .is_none()
    {
        return Err(ApiError::Metadata(
            sift_metadata::MetadataError::PrincipalNotFound(PrincipalId(id)),
        ));
    }
    Ok(Json(
        metadata_store(&state)?.list_principal_auth_sessions(PrincipalId(id))?,
    ))
}

pub(super) async fn admin_revoke_auth_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((principal_id, session_id)): Path<(i64, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?.revoke_principal_auth_session(
        PrincipalId(principal_id),
        &session_id,
        metadata_audit_record(
            auth.principal_id,
            "manage_principal.revoke_session",
            "auth_session",
            None,
        ),
    )?;
    state.auth.runtime.invalidate_auth_session(&session_id);
    state
        .sessions
        .disconnect_managed_principal(PrincipalId(principal_id))
        .await;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Revoke,
            principal_id: Some(principal_id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn admin_issue_password_reset(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((principal_id, identity_id)): Path<(i64, i64)>,
) -> ApiResult<Json<IssuedPasswordResetResponse>> {
    ensure_instance_admin(&state, &auth)?;
    let issued = metadata_store(&state)?
        .issue_password_reset(
            PrincipalId(principal_id),
            AuthIdentityId(identity_id),
            auth.principal_id,
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.issue_password_reset",
                "auth_identity",
                Some(identity_id),
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Reset,
            principal_id: Some(principal_id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(IssuedPasswordResetResponse {
        token: issued.token,
        expires_at: issued.expires_at,
    }))
}

pub(super) fn ensure_instance_admin(state: &AppState, auth: &AuthContext) -> ApiResult<()> {
    let principal = metadata_store(state)?
        .principal_by_id(auth.principal_id)?
        .ok_or(ApiError::Unauthorized)?;
    if principal.is_instance_admin && principal.disabled_at.is_none() {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "instance administrator access required".into(),
        ))
    }
}

pub(super) async fn create_tenant_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(tenant): Path<i64>,
    Json(request): Json<CreateTenantInvitationRequest>,
) -> ApiResult<Json<IssuedTenantInvitationResponse>> {
    let tenant = TenantId(tenant);
    if !is_tenant_admin(&auth, tenant) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    let now = chrono::Utc::now();
    if request.expires_at <= now || request.expires_at > now + chrono::Duration::days(30) {
        return Err(ApiError::BadRequest(
            "invitation expiry must be within the next 30 days".into(),
        ));
    }
    let role = match request.role {
        InvitationRole::Admin => sift_metadata::MembershipRole::Admin,
        InvitationRole::Member => sift_metadata::MembershipRole::Member,
        InvitationRole::Viewer => sift_metadata::MembershipRole::Viewer,
    };
    let issued = metadata_store(&state)?
        .issue_tenant_invitation(
            tenant,
            role,
            auth.principal_id,
            request.target_principal_id.map(PrincipalId),
            request.expires_at,
            metadata_audit_record(
                auth.principal_id,
                "tenant_invitation.create",
                "tenant_invitation",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManageTenantInvitation {
            action: sift_protocol::IdentityAdminAction::Create,
            tenant_id: tenant.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(IssuedTenantInvitationResponse {
        invitation_id: issued.invitation.id.0,
        token: issued.token,
        expires_at: issued.invitation.expires_at,
    }))
}

pub(super) async fn list_tenant_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(tenant): Path<i64>,
) -> ApiResult<Json<Vec<sift_metadata::TenantInvitation>>> {
    let tenant = TenantId(tenant);
    if !is_tenant_admin(&auth, tenant) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    Ok(Json(
        metadata_store(&state)?.list_tenant_invitations(tenant)?,
    ))
}

pub(super) async fn revoke_tenant_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((tenant, id)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant = TenantId(tenant);
    if !is_tenant_admin(&auth, tenant) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    metadata_store(&state)?.revoke_tenant_invitation(
        TenantInvitationId(id),
        metadata_audit_record(
            auth.principal_id,
            "tenant_invitation.revoke",
            "tenant_invitation",
            Some(id),
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageTenantInvitation {
            action: sift_protocol::IdentityAdminAction::Revoke,
            tenant_id: tenant.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn accept_tenant_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<AcceptTenantInvitationRequest>,
) -> ApiResult<Json<TenantMembership>> {
    let membership = metadata_store(&state)?
        .accept_tenant_invitation(
            &request.token,
            auth.principal_id,
            metadata_audit_record(
                auth.principal_id,
                "tenant_invitation.accept",
                "tenant_invitation",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManageTenantInvitation {
            action: sift_protocol::IdentityAdminAction::Link,
            tenant_id: membership.tenant.id.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(membership))
}

pub(super) async fn register_principal_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<RegisterPrincipalKeyRequest>,
) -> ApiResult<Json<sift_metadata::PrincipalKey>> {
    use base64::Engine as _;
    use sha2::Digest as _;

    if request.label.trim().is_empty() || request.label.len() > 100 {
        return Err(ApiError::BadRequest(
            "key label must be between 1 and 100 characters".into(),
        ));
    }
    let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(request.public_key)
        .map_err(|_| ApiError::BadRequest("invalid Ed25519 public key encoding".into()))?;
    if public_key.len() != 32 {
        return Err(ApiError::BadRequest(
            "Ed25519 public key must be exactly 32 bytes".into(),
        ));
    }
    ed25519_dalek::VerifyingKey::from_bytes(
        public_key
            .as_slice()
            .try_into()
            .map_err(|_| ApiError::BadRequest("invalid Ed25519 public key".into()))?,
    )
    .map_err(|_| ApiError::BadRequest("invalid Ed25519 public key".into()))?;
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(&public_key))
    );
    let key = metadata_store(&state)?.register_principal_key(
        auth.principal_id,
        &public_key,
        &fingerprint,
        request.label.trim(),
        metadata_audit_record(
            auth.principal_id,
            "principal_key.register",
            "principal_key",
            None,
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipalKey {
            action: sift_protocol::IdentityAdminAction::Create,
            key_id: Some(key.id.0),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(key))
}

pub(super) async fn list_principal_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<Vec<sift_metadata::PrincipalKey>>> {
    Ok(Json(
        metadata_store(&state)?.list_principal_keys(auth.principal_id)?,
    ))
}

pub(super) async fn revoke_principal_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    metadata_store(&state)?.revoke_principal_key(
        PrincipalKeyId(id),
        auth.principal_id,
        metadata_audit_record(
            auth.principal_id,
            "principal_key.revoke",
            "principal_key",
            Some(id),
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipalKey {
            action: sift_protocol::IdentityAdminAction::Revoke,
            key_id: Some(id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn issue_key_challenge(
    State(state): State<AppState>,
    Json(request): Json<KeyChallengeRequest>,
) -> ApiResult<Json<KeyChallengeResponse>> {
    use base64::Engine as _;

    let challenge = metadata_store(&state)?
        .issue_key_challenge(&request.fingerprint)
        .map_err(|_| ApiError::Unauthorized)?;
    let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&challenge.nonce);
    Ok(Json(KeyChallengeResponse {
        message: key_challenge_message(&state.auth.instance_audience, &nonce),
        nonce,
        expires_at: challenge.expires_at,
    }))
}

pub(super) async fn authenticate_key(
    State(state): State<AppState>,
    Json(request): Json<KeyAuthenticateRequest>,
) -> ApiResult<Json<AuthTokensResponse>> {
    use base64::Engine as _;
    use ed25519_dalek::Verifier as _;

    let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&request.nonce)
        .map_err(|_| ApiError::Unauthorized)?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&request.signature)
        .map_err(|_| ApiError::Unauthorized)?;
    let consumed = metadata_store(&state)?
        .consume_key_challenge(&nonce)
        .map_err(|_| ApiError::Unauthorized)?;
    let public_key: [u8; 32] = consumed
        .principal_key
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::Unauthorized)?;
    let signature =
        ed25519_dalek::Signature::from_slice(&signature).map_err(|_| ApiError::Unauthorized)?;
    let verifying_key =
        ed25519_dalek::VerifyingKey::from_bytes(&public_key).map_err(|_| ApiError::Unauthorized)?;
    let nonce_text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&nonce);
    verifying_key
        .verify(
            key_challenge_message(&state.auth.instance_audience, &nonce_text).as_bytes(),
            &signature,
        )
        .map_err(|_| ApiError::Unauthorized)?;
    let tokens = metadata_store(&state)?
        .issue_auth_session(
            consumed.principal_key.principal_id,
            MetadataAuthClientKind::Keypair,
            Some(&consumed.principal_key.label),
            metadata_audit_record(
                consumed.principal_key.principal_id,
                "authenticate.keypair",
                "auth_session",
                None,
            ),
        )
        .await?;
    Ok(Json(auth_tokens_response(tokens)))
}

pub(super) async fn exchange_ssh_proxy_capability(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SshProxyCapabilityExchangeRequest>,
) -> ApiResult<Json<SshProxyAccessGrant>> {
    if state.auth.transport != Transport::SshProxy {
        return Err(ApiError::Forbidden(
            "SSH proxy capability exchange is unavailable on this transport".into(),
        ));
    }
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    if state.auth.runtime.ssh_capability_is_limited(source) {
        return Err(ApiError::TooManyAuthAttempts);
    }
    let metadata = metadata_store(&state)?;
    let issued = match metadata
        .consume_ssh_proxy_capability(
            &request.capability,
            &state.auth.instance_audience,
            &state.auth.daemon_generation,
            NewOperationAudit {
                actor_principal_id: None,
                action: "authenticate.ssh_capability".into(),
                target: "auth_session".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )
        .await
    {
        Ok(issued) => issued,
        Err(error) => {
            tracing::warn!(%error, "SSH proxy capability exchange was denied");
            state.auth.runtime.record_ssh_capability_failure(source);
            record_auth_failure(metadata, "authenticate.ssh_capability", "denied")?;
            state.sessions.push_operation_full(
                Operation::Authenticate {
                    method: sift_protocol::AuthenticationMethod::SshCapability,
                },
                OperationStatus::Failed,
                None,
                Some("authentication_denied".into()),
                None,
                Some("authentication denied".into()),
            );
            return Err(ApiError::Unauthorized);
        }
    };
    state.auth.runtime.clear_ssh_capability_failures(source);
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::SshCapability,
        },
        OperationStatus::Succeeded,
        Some(issued.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(SshProxyAccessGrant {
        access_token: issued.access_token,
        expires_at: issued.access_expires_at,
        principal_id: issued.principal_id.0,
        daemon_generation: issued.daemon_generation,
    }))
}

pub(super) fn key_challenge_message(audience: &str, nonce: &str) -> String {
    format!("sift-key-auth-v1\n{audience}\n{nonce}")
}

pub(super) async fn whoami(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<WhoAmIResponse>> {
    let metadata = metadata_store(&state)?;
    let principal = metadata
        .principal_by_id(auth.principal_id)?
        .ok_or(ApiError::Unauthorized)?;
    let github_login = metadata
        .list_auth_identities(auth.principal_id)?
        .into_iter()
        .find(|identity| {
            identity.method == sift_metadata::AuthIdentityMethod::Github
                && identity.disabled_at.is_none()
        })
        .and_then(|identity| identity.provider_login);
    let memberships = auth
        .tenants
        .iter()
        .map(|membership| AuthTenantMembership {
            tenant_id: membership.tenant.id.0,
            tenant_name: membership.tenant.name.clone(),
            role: match membership.role {
                sift_metadata::MembershipRole::Owner => "owner",
                sift_metadata::MembershipRole::Admin => "admin",
                sift_metadata::MembershipRole::Member => "member",
                sift_metadata::MembershipRole::Viewer => "viewer",
            }
            .into(),
        })
        .collect();
    Ok(Json(WhoAmIResponse {
        principal: AuthPrincipal {
            id: principal.id.0,
            display_name: principal.display_name,
            email: principal.email,
            avatar_url: principal.avatar_url,
            is_instance_admin: principal.is_instance_admin,
        },
        memberships,
        github_login,
        auth_session_id: auth.auth_session_id,
    }))
}

pub(super) fn auth_tokens_response(tokens: sift_metadata::IssuedAuthTokens) -> AuthTokensResponse {
    AuthTokensResponse {
        access_token: tokens.access_token,
        access_expires_at: tokens.access_expires_at,
        refresh_token: tokens.refresh_token,
        refresh_expires_at: tokens.refresh_expires_at,
    }
}

pub(super) fn auth_login_response(
    tokens: sift_metadata::IssuedAuthTokens,
    web: bool,
) -> ApiResult<Response> {
    if !web {
        return Ok(Json(auth_tokens_response(tokens)).into_response());
    }
    let csrf = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let access_cookie = format!(
        "sift_access={}; Path=/; Max-Age=900; Secure; HttpOnly; SameSite=Lax",
        tokens.access_token
    );
    let refresh_cookie = format!(
        "sift_refresh={}; Path=/v1/auth/refresh; Max-Age=2592000; Secure; HttpOnly; SameSite=Strict",
        tokens.refresh_token
    );
    let csrf_cookie = format!("sift_csrf={csrf}; Path=/; Max-Age=2592000; Secure; SameSite=Strict");
    let mut response = Json(WebAuthResponse {
        access_expires_at: tokens.access_expires_at,
        refresh_expires_at: tokens.refresh_expires_at,
        csrf_token: csrf,
    })
    .into_response();
    for cookie in [access_cookie, refresh_cookie, csrf_cookie] {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie)
                .map_err(|error| ApiError::Internal(format!("invalid auth cookie: {error}")))?,
        );
    }
    Ok(response)
}

pub(super) fn logout_response(clear_cookies: bool) -> Response {
    let mut response = Json(json!({"ok": true})).into_response();
    if clear_cookies {
        for cookie in [
            "sift_access=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax",
            "sift_refresh=; Path=/v1/auth/refresh; Max-Age=0; Secure; HttpOnly; SameSite=Strict",
            "sift_csrf=; Path=/; Max-Age=0; Secure; SameSite=Strict",
        ] {
            response
                .headers_mut()
                .append(header::SET_COOKIE, HeaderValue::from_static(cookie));
        }
    }
    response
}

pub(super) fn record_auth_failure(
    metadata: &MetadataStore,
    action: &str,
    code: &str,
) -> ApiResult<()> {
    metadata.record_operation_audit(NewOperationAudit {
        actor_principal_id: None,
        action: action.into(),
        target: "auth_session".into(),
        target_id: None,
        status: "failed".into(),
        result_code: Some(code.into()),
        row_count: None,
        error_message: Some("authentication denied".into()),
        correlation_id: crate::correlation::current(),
    })?;
    Ok(())
}
