//! Captured human authority and redacted receipt inspection.
use super::*;
use crate::extension_dispatch::{ActionAuthorization, DispatchContext};
use sift_protocol::{ExtensionExecutionReceipt, ToolContext};

pub(super) fn source_fingerprint(
    state: &AppState,
    auth: &AuthContext,
    target: &ToolContext,
) -> ApiResult<String> {
    let metadata = metadata_store_cloned(state)?;
    let metadata_fingerprint = metadata.extension_source_fingerprint(auth.principal_id, target)?;
    let connection = if let Some(id) = target.connection_id.as_deref() {
        let (session, connection) = id
            .split_once(':')
            .ok_or_else(|| ApiError::BadRequest("invalid connection identity".into()))?;
        let session = session
            .parse::<u64>()
            .map_err(|_| ApiError::BadRequest("invalid session identity".into()))?;
        let connection = connection
            .parse::<u64>()
            .map_err(|_| ApiError::BadRequest("invalid connection identity".into()))?;
        let entry = state.sessions.conn_entry(
            sift_protocol::SessionId(session),
            sift_protocol::ConnectionId(connection),
        )?;
        Some(
            json!({"daemon":state.auth.daemon_generation,"source":entry.execution.review_id,"configuration":entry.configuration}),
        )
    } else {
        None
    };
    crate::automation::fingerprint(
        &json!({"metadata":metadata_fingerprint,"connection":connection}),
    )
    .map_err(|_| ApiError::BadRequest("invalid extension source identity".into()))
}

pub(super) fn context(
    state: &AppState,
    auth: &AuthContext,
    headers: HeaderMap,
    target: ToolContext,
) -> ApiResult<DispatchContext> {
    let authorization = tool_authorization_scope(state, auth, &target)?;
    let credential_proof = if auth.database_credential {
        let token = bearer_from_headers(&headers)
            .or_else(|| cookie_value(&headers, "sift_access"))
            .ok_or(ApiError::Unauthorized)?;
        Some(
            metadata_store_cloned(state)?
                .extension_verified_credential_reference(auth.principal_id, token)?,
        )
    } else {
        None
    };
    let metadata_fingerprint =
        metadata_store_cloned(state)?.extension_source_fingerprint(auth.principal_id, &target)?;
    let source = source_fingerprint(state, auth, &target)?;
    let expected = source.clone();
    let state = state.clone();
    let actor = auth.principal_id;
    let captured = target.clone();
    let reauthorize: ActionAuthorization = Arc::new(move |classification| {
        let state = state.clone();
        let headers = headers.clone();
        let captured = captured.clone();
        let expected = expected.clone();
        Box::pin(async move {
            if state.shutdown.is_draining() {
                return Err(sift_plugin_host::SupervisorError::AdmissionRefused);
            }
            let fresh = resolve_auth_context_blocking(state.clone(), headers)
                .await
                .map_err(|_| sift_plugin_host::SupervisorError::AdmissionRefused)?;
            metadata_blocking(move || {
                if fresh.principal_id != actor {
                    return Err(ApiError::Unauthorized);
                }
                let current = authorized_tool_context(&state, &fresh, captured.clone())?;
                if current != captured || source_fingerprint(&state, &fresh, &current)? != expected
                {
                    return Err(ApiError::Forbidden("extension source changed".into()));
                }
                crate::authorization::authorize_extension(
                    &tool_authorization_scope(&state, &fresh, &current)?,
                    classification,
                )
                .map_err(|_| ApiError::Forbidden("extension authority changed".into()))
            })
            .await
            .map_err(|_| sift_plugin_host::SupervisorError::AdmissionRefused)
        })
    });
    Ok(DispatchContext {
        authorization,
        principal_id: actor,
        target,
        correlation_id: crate::correlation::current()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        credential_proof,
        metadata_fingerprint: Some(metadata_fingerprint),
        source_fingerprint: Some(source),
        reauthorize: Some(reauthorize),
    })
}

pub(super) fn disclose(
    state: &AppState,
    auth: &AuthContext,
    row: sift_metadata::ExtensionExecutionRecord,
) -> ApiResult<ExtensionExecutionReceipt> {
    let metadata = metadata_store_cloned(state)?;
    if auth
        .token_tenant_scope
        .is_some_and(|scope| row.receipt.tenant_id != Some(scope))
    {
        return Err(ApiError::Unauthorized);
    }

    // No native connection is needed, and no saved output is returned. Existing
    // room/profile scope remains authoritative; removed objects add no disclosure.
    if let Some(room) = row.context.room_id {
        match metadata.get_room(RoomId(room)) {
            Ok(_)
                if metadata
                    .get_room_member(RoomId(room), auth.principal_id)?
                    .is_none() =>
            {
                return Err(ApiError::Unauthorized)
            }
            Ok(_) | Err(sift_metadata::MetadataError::RoomNotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(profile) = row.context.profile_id {
        match metadata
            .get_connection_profile_for_principal(ConnectionProfileId(profile), auth.principal_id)
        {
            Ok(_) | Err(sift_metadata::MetadataError::ConnectionProfileNotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if row
        .context
        .tenant_id
        .is_some_and(|id| !auth.tenants.iter().any(|t| t.tenant.id.0 == id))
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(row.receipt)
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ReceiptQuery {
    approval_id: Option<String>,
    request_id: Option<uuid::Uuid>,
}

pub(super) async fn list(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<ReceiptQuery>,
) -> ApiResult<Json<Vec<ExtensionExecutionReceipt>>> {
    let auth = auth.map(|Extension(a)| a).ok_or(ApiError::Unauthorized)?;
    let owner = auth.principal_id;
    let audit_state = state.clone();
    let result = metadata_blocking(move || {
        let store = metadata_store_cloned(&state)?;
        let rows = match (query.approval_id, query.request_id) {
            (Some(id), None) => store
                .extension_execution_for_approval(&id, owner)?
                .into_iter()
                .collect(),
            (None, Some(id)) => store
                .extension_execution_for_request(id, owner)?
                .into_iter()
                .collect(),
            (None, None) => store.extension_executions(owner)?,
            _ => {
                return Err(ApiError::BadRequest(
                    "select one receipt lookup identity".into(),
                ))
            }
        };
        Ok(rows
            .into_iter()
            .filter_map(|row| disclose(&state, &auth, row).ok())
            .collect::<Vec<_>>())
    })
    .await;
    Ok(Json(finish_operation_as(
        &audit_state.sessions,
        Operation::InspectExtensionExecutions { execution_id: None },
        result,
        Some(owner.0),
        |r| Some(r.len() as i64),
    )?))
}

pub(super) async fn get(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<ExtensionExecutionReceipt>> {
    let auth = auth.map(|Extension(a)| a).ok_or(ApiError::Unauthorized)?;
    let owner = auth.principal_id;
    let audit_state = state.clone();
    let result = metadata_blocking(move || {
        let row = metadata_store_cloned(&state)?.extension_execution(id, owner)?;
        disclose(&state, &auth, row)
    })
    .await;
    Ok(Json(finish_operation_as(
        &audit_state.sessions,
        Operation::InspectExtensionExecutions {
            execution_id: Some(id),
        },
        result,
        Some(owner.0),
        |_| None,
    )?))
}
