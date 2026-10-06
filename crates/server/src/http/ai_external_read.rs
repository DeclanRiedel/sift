//! Selected-source reads remain under Sift's lease, quota and cancellation.
use super::*;
use sift_protocol::{
    AiExternalSourceProof, InvokeAiExternalReadRequest, InvokeAiExternalReadResponse,
};

async fn credential(
    state: &AppState,
    auth: &AuthContext,
    run: &sift_metadata::AiAuthorizedToolRun,
    proof: AiExternalSourceProof,
) -> ApiResult<(sift_metadata::AiExternalCredential, Vec<String>)> {
    let metadata = metadata_store_cloned(state)?;
    let chat = metadata.get_ai_chat(run.chat_id, auth.principal_id).await?;
    let tenant = TenantId(chat.tenant_id);
    ensure_tenant(auth, tenant)?;
    if chat.visibility == sift_protocol::AiVisibility::RoomPublic {
        Ok(metadata
            .ai_external_room_credential(
                tenant,
                RoomId(chat.room_id.ok_or(ApiError::Unauthorized)?),
                auth.principal_id,
                proof,
            )
            .await?)
    } else {
        let value = metadata
            .ai_external_private_credential(tenant, auth.principal_id, proof)
            .await?;
        let aliases = value
            .source
            .definition
            .tools
            .iter()
            .filter(|tool| tool.policy != sift_protocol::AiExternalToolPolicy::Unavailable)
            .map(|tool| tool.alias.clone())
            .collect();
        Ok((value, aliases))
    }
}

pub(super) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<InvokeAiExternalReadRequest>,
) -> ApiResult<Json<InvokeAiExternalReadResponse>> {
    ai_enabled(&state)?;
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    check_ai_run_scope(&state, &auth, id).await?;
    let work = state.sessions.register_ai_work(id)?;
    let guard = work.token.clone().drop_guard();
    let shutdown = state.shutdown.track_query();
    let task = tokio::spawn(async move {
        let _shutdown = shutdown;
        let metadata = metadata_store_cloned(&state)?;
        let mut reserved = false;
        let mut result:ApiResult<InvokeAiExternalReadResponse> = async {
            let run = metadata.ai_tool_run(id, auth.principal_id, request.lease_token).await?;
            let arguments = serde_json::to_vec(&request.arguments).map_err(|_|ApiError::BadRequest("External arguments are invalid".into()))?;
            if arguments.len()>64*1024 { return Err(ApiError::BadRequest("External arguments exceed their limit".into())); }
            let (proof, _) = metadata.ai_external_run_source(id, auth.principal_id, request.lease_token, request.source_id).await?;
            let invocation = sift_metadata::AiExternalReadInvocation {run_id:id,actor:auth.principal_id,lease:request.lease_token,call_id:request.call_id,proof:proof.clone(),alias:request.tool_alias.clone()};
            metadata.reserve_ai_external_read(invocation.clone(),state.auth.ai.max_tool_calls_per_run,format!("{:x}",Sha256::digest(arguments))).await?;
            reserved = true;
            let remaining = (run.started_at + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)) - chrono::Utc::now())
                .to_std().map_err(|_|ApiError::Forbidden("AI run time limit reached".into()))?;
            let read = async {
                let fresh = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
                if fresh.principal_id!=auth.principal_id {return Err(ApiError::Unauthorized);}
                check_ai_run_scope(&state, &fresh, id).await?;
                let (mut credential, aliases) = credential(&state, &fresh, &run, proof.clone()).await?;
                if !aliases.contains(&request.tool_alias) { return Err(ApiError::Forbidden("External tool is outside the reviewed room grant".into())); }
                credential.source.definition.tools.retain(|tool|aliases.contains(&tool.alias));
                let value = crate::ai_external_gateway::read(&credential.source.definition, credential.bearer_token,
                    &request.tool_alias, request.arguments.clone(), || async {
                        if work.token.is_cancelled() || chrono::Utc::now() >= run.started_at + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)) { return Err("AI run stopped or reached its time limit".into()); }
                        let fresh = resolve_auth_context_blocking(state.clone(), headers.clone()).await.map_err(|_|"AI authentication changed")?;
                        if fresh.principal_id!=auth.principal_id {return Err("AI authentication changed".into());}
                        check_ai_run_scope(&state, &fresh, id).await.map_err(|_|"AI scope changed")?;
                        let (current, _) = metadata.ai_external_run_source(id, fresh.principal_id, request.lease_token, proof.source_id).await.map_err(|_|"External source authority changed")?;
                        if current!=proof { return Err("External source authority changed".into()); }
                        Ok(())
                    }).await.map_err(ApiError::BadRequest)?;
                Ok::<_,ApiError>(value)
            };
            let value = crate::ai_cancellation::scope(work.token.clone(), async {
                tokio::select! {
                    biased;
                    _=work.token.cancelled()=>Err(ApiError::Forbidden("AI run stopped".into())),
                    result=tokio::time::timeout(remaining,read)=> result.map_err(|_|ApiError::Forbidden("AI run time limit reached".into()))?,
                }
            }).await?;
            if serde_json::to_vec(&value).map_err(|_|ApiError::Internal("External result cannot be encoded".into()))?.len() as u64 > state.auth.ai.max_tool_result_bytes {
                return Err(ApiError::BadRequest("External result exceeds the instance limit".into()));
            }
            let fresh = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
            if fresh.principal_id!=auth.principal_id {return Err(ApiError::Unauthorized);}
            check_ai_run_scope(&state, &fresh, id).await?;
            if work.token.is_cancelled() || chrono::Utc::now()>=run.started_at + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)) {
                return Err(ApiError::Forbidden("AI run stopped or reached its time limit".into()));
            }
            metadata.complete_ai_external_read(invocation, value.clone()).await?;
            let fresh = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
            if fresh.principal_id!=auth.principal_id || work.token.is_cancelled() {return Err(ApiError::Unauthorized);}
            check_ai_run_scope(&state, &fresh, id).await?;
            metadata.ai_external_run_source(id, fresh.principal_id, request.lease_token, proof.source_id).await?;
            Ok(InvokeAiExternalReadResponse {call_id:request.call_id, source:proof, tool_alias:request.tool_alias.clone(), result:value})
        }.await;
        if reserved && result.is_err() {
            if let Err(error) = metadata
                .finish_ai_tool_call(
                    id,
                    auth.principal_id,
                    request.lease_token,
                    request.call_id,
                    false,
                )
                .await
            {
                result = Err(error.into());
            }
        }
        state.sessions.push_operation_full(
            Operation::Ai {
                action: "external_read".into(),
                chat_id: None,
                run_id: Some(id),
            },
            if result.is_ok() {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
            Some(auth.principal_id.0),
            None,
            None,
            result
                .is_err()
                .then(|| "AI external read denied or did not complete".into()),
        );
        result
    });
    let result = task
        .await
        .map_err(|_| ApiError::Internal("External read did not settle".into()))?;
    guard.disarm();
    Ok(Json(result?))
}
