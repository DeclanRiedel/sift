//! Reviewed external intents stage local drafts; no remote transport is used.
use super::*;
use sift_protocol::{
    AiExternalLocalDraft, AiExternalProposalDetail, StageAiExternalProposalRequest,
};

pub(super) async fn stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<StageAiExternalProposalRequest>,
) -> ApiResult<Json<AiExternalProposalDetail>> {
    super::ai::ai_enabled(&state)?;
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
    let id = super::ai::ai_chat_id(&raw)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    super::ai::check_ai_run_scope(&state, &auth, id).await?;
    let work = state.sessions.register_ai_work(id)?;
    let waiter = work.token.clone().drop_guard();
    let shutdown = state.shutdown.track_query();
    let task = tokio::spawn(async move {
        let _shutdown = shutdown;
        let _work = work;
        let metadata = metadata_store_cloned(&state)?;
        let result: ApiResult<_> = async {
            let run = metadata
                .ai_tool_run(id, auth.principal_id, request.lease_token)
                .await?;
            let (_, source) = metadata
                .ai_external_run_source(
                    id,
                    auth.principal_id,
                    request.lease_token,
                    request.source_id,
                )
                .await?;
            if !source.definition.tools.iter().any(|tool| {
                tool.alias == request.tool_alias && tool.policy == request.draft.policy()
            }) {
                return Err(ApiError::Forbidden(
                    "Reviewed source intent does not match this local draft kind".into(),
                ));
            }
            if _work.token.is_cancelled() {
                return Err(ApiError::Forbidden("AI run stopped".into()));
            }
            let intent = Some(sift_metadata::AiExternalProposalIntent {
                source_id: request.source_id,
                alias: request.tool_alias,
            });
            match request.draft {
                AiExternalLocalDraft::Query {
                    base_revision,
                    proposed_sql,
                } => {
                    let Json(detail) = super::ai::stage_ai_query_proposal_with_intent(
                        State(state.clone()),
                        headers.clone(),
                        Path(raw),
                        Json(sift_protocol::StageAiQueryProposalRequest {
                            client_request_id: request.client_request_id,
                            lease_token: request.lease_token,
                            target: run.context.target,
                            base_revision,
                            proposed_sql,
                        }),
                        intent,
                    )
                    .await?;
                    Ok(Json(AiExternalProposalDetail::Query {
                        detail: Box::new(detail),
                    }))
                }
                AiExternalLocalDraft::Database { draft } => {
                    let Json(detail) = super::ai_database::stage_ai_database_change_with_intent(
                        State(state.clone()),
                        headers.clone(),
                        Path(raw),
                        Json(sift_protocol::StageAiDatabaseProposalRequest {
                            client_request_id: request.client_request_id,
                            lease_token: request.lease_token,
                            draft: *draft,
                        }),
                        intent,
                    )
                    .await?;
                    Ok(Json(AiExternalProposalDetail::Database {
                        detail: Box::new(detail),
                    }))
                }
            }
        }
        .await;
        state.sessions.push_operation_full(
            Operation::Ai {
                action: "stage_external_proposal".into(),
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
                .then(|| "Reviewed local source intent could not be staged".into()),
        );
        if result.is_ok() {
            let fresh = resolve_auth_context_blocking(state.clone(), headers).await?;
            super::ai::check_ai_run_scope(&state, &fresh, id).await?;
            metadata
                .ai_external_run_source(
                    id,
                    fresh.principal_id,
                    request.lease_token,
                    request.source_id,
                )
                .await?;
        }
        result
    });
    let result = task
        .await
        .map_err(|_| ApiError::Internal("Local proposal task failed".into()))?;
    waiter.disarm();
    result
}
