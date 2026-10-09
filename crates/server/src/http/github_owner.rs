use super::*;

pub(super) async fn auth_methods(
    State(state): State<AppState>,
) -> Json<sift_protocol::AuthMethodsResponse> {
    Json(sift_protocol::AuthMethodsResponse {
        github_sign_in: state.auth.github.is_some(),
        github_owner_device: state.auth.github_owner_device.is_some()
            && state.auth.deployment == DeploymentPolicy::Personal
            && state.auth.transport == Transport::Loopback
            && state.auth.loopback_bypass,
    })
}

fn owner_device<'a>(
    state: &'a AppState,
    auth: &AuthContext,
) -> ApiResult<&'a crate::github_device::GithubOwnerDevice> {
    if !auth.trusted_local
        || state.auth.deployment != DeploymentPolicy::Personal
        || state.auth.transport != Transport::Loopback
        || !state.auth.loopback_bypass
    {
        return Err(ApiError::Forbidden(
            "GitHub owner setup requires verified local ownership".into(),
        ));
    }
    if !metadata_store(state)?.can_link_local_github_owner(auth.principal_id)? {
        return Err(ApiError::Forbidden(
            "Active local instance ownership required".into(),
        ));
    }
    state.auth.github_owner_device.as_ref().ok_or_else(|| {
        ApiError::BadRequest(
            "Configure an OAuth App client ID and enable its device flow before linking GitHub"
                .into(),
        )
    })
}

pub(super) async fn github_owner_device_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<sift_protocol::GithubOwnerDeviceStartResponse>> {
    let start = owner_device(&state, &auth)?
        .start(auth.principal_id)
        .await
        .map_err(|message| ApiError::BadRequest(message.into()))?;
    if let Err(error) = metadata_store(&state)?.record_operation_audit(metadata_audit_record(
        auth.principal_id,
        "authenticate.github.owner_start",
        "principal",
        Some(auth.principal_id.0),
    )) {
        let _ = owner_device(&state, &auth)?.cancel(&start.handoff_token, auth.principal_id);
        return Err(error.into());
    }
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Github,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(start))
}

pub(super) async fn github_owner_device_poll(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<GithubNativeAuthExchangeRequest>,
) -> ApiResult<Json<sift_protocol::GithubOwnerDevicePollResponse>> {
    let device = owner_device(&state, &auth)?;
    let outcome = match device.poll(&request.handoff_token, auth.principal_id).await {
        Ok(outcome) => outcome,
        Err(message) => {
            record_auth_failure(
                metadata_store(&state)?,
                "authenticate.github.owner_link",
                "device_authorization_failed",
            )?;
            state.sessions.push_operation_local(
                Operation::Authenticate {
                    method: sift_protocol::AuthenticationMethod::Github,
                },
                OperationStatus::Failed,
                Some(auth.principal_id.0),
                Some("device_authorization_failed".into()),
                None,
                Some(message.into()),
            );
            return Err(ApiError::BadRequest(message.into()));
        }
    };
    match outcome {
        crate::github_device::DevicePoll::Pending(response) => Ok(Json(response)),
        crate::github_device::DevicePoll::Authorized(profile) => {
            let login = profile.login.clone();
            let linked = metadata_store(&state)?.link_github_owner(
                auth.principal_id,
                profile,
                metadata_audit_record(
                    auth.principal_id,
                    "authenticate.github.owner_link",
                    "principal",
                    Some(auth.principal_id.0),
                ),
            )?;
            if linked.is_none() {
                record_auth_failure(
                    metadata_store(&state)?,
                    "authenticate.github.owner_link",
                    "identity_conflict",
                )?;
                return Err(ApiError::Forbidden(
                    "This GitHub account cannot replace the instance owner's declared identity"
                        .into(),
                ));
            }
            state.sessions.push_operation_local(
                Operation::ManagePrincipal {
                    action: sift_protocol::IdentityAdminAction::Link,
                    principal_id: Some(auth.principal_id.0),
                },
                OperationStatus::Succeeded,
                Some(auth.principal_id.0),
                None,
                None,
                None,
            );
            Ok(Json(sift_protocol::GithubOwnerDevicePollResponse {
                completed: true,
                interval_secs: 0,
                github_login: Some(login),
            }))
        }
    }
}

pub(super) async fn github_owner_device_cancel(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<GithubNativeAuthExchangeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    owner_device(&state, &auth)?
        .cancel(&request.handoff_token, auth.principal_id)
        .map_err(|message| ApiError::BadRequest(message.into()))?;
    metadata_store(&state)?.record_operation_audit(metadata_audit_record(
        auth.principal_id,
        "authenticate.github.owner_cancel",
        "principal",
        Some(auth.principal_id.0),
    ))?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Github,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_setup_requires_local_proof_and_never_allows_team_or_ssh_claims() {
        let metadata =
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::new()))
                .unwrap();
        metadata.bootstrap_local("Owner").unwrap();
        let peer = metadata
            .create_principal("fixture-peer", "Peer", None)
            .unwrap();
        let mut state = AppState {
            sessions: SessionStore::new(crate::registry::DriverRegistry::builder().build()),
            rooms: RoomRuntime::default(),
            metadata: Some(metadata),
            shutdown: Default::default(),
            auth: AuthState {
                github_owner_device: Some(
                    crate::github_device::GithubOwnerDevice::new("fixture".into()).unwrap(),
                ),
                ..Default::default()
            },
        };
        let mut auth = AuthContext {
            principal_id: PrincipalId(1),
            tenants: vec![],
            auth_session_id: None,
            cookie_authenticated: false,
            access_expires_at: None,
            trusted_local: true,
        };
        assert!(owner_device(&state, &auth).is_ok());
        auth.trusted_local = false;
        assert!(owner_device(&state, &auth).is_err());
        auth.trusted_local = true;
        auth.principal_id = peer.id;
        assert!(owner_device(&state, &auth).is_err());
        auth.principal_id = PrincipalId(1);
        state.auth.deployment = DeploymentPolicy::Team;
        assert!(owner_device(&state, &auth).is_err());
        state.auth.deployment = DeploymentPolicy::Personal;
        state.auth.transport = Transport::SshProxy;
        assert!(owner_device(&state, &auth).is_err());
        state.auth.transport = Transport::Network;
        assert!(owner_device(&state, &auth).is_err());
    }
}
