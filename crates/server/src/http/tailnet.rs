use super::*;
use sift_api_types::{TailnetProbeReport, TailnetProbeRequest, TailnetStatus};

pub(super) async fn tailnet_host_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TailnetProbeRequest>,
) -> ApiResult<Json<sift_api_types::TailnetHostKey>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    ensure_instance_admin(&state, &auth)?;
    let result = crate::tailnet::scan_host_key(request).await;
    audit_tailnet(&state, auth.principal_id, "scan_host_key", result.is_ok());
    result.map(Json)
}

pub(super) async fn validate_connection_candidate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpsertConnectionProfileRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(request.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    admit_resolved_tenant(
        &state,
        &auth,
        Some(tenant),
        sift_protocol::RateLimitClass::Control,
        "/v1/metadata/connections/validate",
    )?;
    let mut provider_configuration = request.configuration.clone();
    if crate::tailnet::settings(&provider_configuration)?.is_some() {
        ensure_instance_admin(&state, &auth)?;
        provider_configuration
            .as_object_mut()
            .unwrap()
            .remove("sift_network");
    }
    let registered = state
        .sessions
        .registry()
        .get_provider(&request.provider_id)?;
    let schema = registered
        .provider
        .descriptor()
        .configuration_schema
        .clone();
    let validator = jsonschema::draft202012::new(&schema)
        .map_err(|_| ApiError::Internal("Invalid provider schema".into()))?;
    if !validator.is_valid(&provider_configuration) {
        return Err(ApiError::BadRequest(
            "Invalid provider configuration".into(),
        ));
    }
    let metadata = metadata_store(&state)?;
    let mut credentials = std::collections::HashMap::new();
    if let Some(value) = request.credentials {
        let fields = value
            .as_object()
            .ok_or_else(|| ApiError::BadRequest("Invalid credentials".into()))?;
        for (key, value) in fields {
            if value.is_null() {
                continue;
            }
            let value = value
                .as_str()
                .ok_or_else(|| ApiError::BadRequest("Credential values must be strings".into()))?;
            credentials.insert(key.clone(), value.as_bytes().to_vec());
        }
    } else {
        let profiles = metadata.list_connection_profiles(tenant)?;
        if let Some(existing) = profiles.iter().find(|p| p.name == request.name) {
            if existing.configuration != request.configuration {
                require_tenant_admin(&auth, tenant)?;
            }
            if existing.provider_id != request.provider_id {
                return Err(ApiError::BadRequest(
                    "Cannot reuse credentials across providers".into(),
                ));
            }
            credentials = metadata
                .resolve_provider_connection(tenant, auth.principal_id, existing.id)
                .await?
                .1;
        }
    }
    let result = state
        .sessions
        .test_provider_configuration(
            request.provider_id,
            request.configuration,
            credentials,
            tenant.0,
        )
        .await;
    state.sessions.push_operation_full(
        Operation::Metadata {
            action: "validate".into(),
            target: "connection_profile".into(),
            id: None,
        },
        if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    result?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn tailnet_serve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<sift_api_types::TailnetServeRequest>,
) -> ApiResult<Json<sift_api_types::TailnetServeReport>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    ensure_instance_admin(&state, &auth)?;
    if matches!(
        request.action,
        sift_api_types::TailnetServeAction::Apply | sift_api_types::TailnetServeAction::Remove
    ) && request.expected_instance_id.as_deref() != Some(state.auth.instance_id.as_str())
    {
        return Err(ApiError::BadRequest(
            "Sift backend changed; preview Serve again before confirming".into(),
        ));
    }
    let action = format!("serve_{:?}", request.action).to_lowercase();
    let result = crate::tailnet::serve(request, state.auth.instance_id.clone()).await;
    state.sessions.push_operation_full(
        Operation::Metadata {
            action,
            target: "tailnet".into(),
            id: None,
        },
        if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    result.map(|mut report| {
        report.instance_id = state.auth.instance_id.clone();
        Json(report)
    })
}

pub(super) async fn tailnet_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<TailnetStatus>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    ensure_instance_admin(&state, &auth)?;
    let result = crate::tailnet::status().await;
    audit_tailnet(&state, auth.principal_id, "discover", result.is_ok());
    result.map(Json)
}

pub(super) async fn tailnet_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TailnetProbeRequest>,
) -> ApiResult<Json<TailnetProbeReport>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    ensure_instance_admin(&state, &auth)?;
    let result = crate::tailnet::probe(request).await;
    audit_tailnet(
        &state,
        auth.principal_id,
        "probe",
        result.as_ref().is_ok_and(|report| report.reachable),
    );
    result.map(Json)
}

fn audit_tailnet(state: &AppState, actor: PrincipalId, action: &str, success: bool) {
    state.sessions.push_operation_full(
        Operation::Metadata {
            action: action.into(),
            target: "tailnet".into(),
            id: None,
        },
        if success {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(actor.0),
        None,
        None,
        None,
    );
}
