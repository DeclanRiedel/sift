//! Human room proposal application lives in the retained server document owner.
use super::*;
use sift_protocol::{
    AiRoomQueryProposalCommit, CommitAiRoomQueryProposalRequest, CrdtUpdate, DocumentVersion,
    ReplicaId,
};
use uuid::Uuid;

async fn prepare(
    state: &AppState,
    auth: &AuthContext,
    chat: Uuid,
    proposal: Uuid,
    request: CommitAiRoomQueryProposalRequest,
) -> ApiResult<sift_metadata::PreparedAiRoomQueryCommit> {
    super::ai::check_ai_chat_scope(state, auth, chat).await?;
    Ok(metadata_store_cloned(state)?
        .prepare_ai_room_query_commit(
            chat,
            proposal,
            auth.principal_id,
            auth.auth_session_id.clone(),
            request,
        )
        .await?)
}

pub(super) async fn commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((chat, proposal)): Path<(String, String)>,
    Json(request): Json<CommitAiRoomQueryProposalRequest>,
) -> ApiResult<Json<AiRoomQueryProposalCommit>> {
    super::ai::ai_enabled(&state)?;
    let configured = state.sessions.request_timeout();
    let limit = std::time::Duration::from_secs(u64::from(state.auth.ai.max_run_secs));
    let timeout = if configured.is_zero() {
        limit
    } else {
        configured.min(limit)
    };
    let deadline = std::time::Instant::now() + timeout;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    super::ai_database::captured_review_caller(&state, &auth, &headers)?;
    let chat = super::ai::ai_chat_id(&chat)?;
    let proposal = super::ai::ai_chat_id(&proposal)?;
    let actor = auth.principal_id;
    let delivery_state = state.clone();
    let delivery_headers = headers.clone();
    let result = super::execution::owned_audited_operation(
        state.clone(),
        Operation::Ai {
            action: "commit_room_query_proposal".into(),
            chat_id: Some(chat),
            run_id: None,
        },
        Some(actor.0),
        Box::pin(async move {
            let prepared = prepare(&state, &auth, chat, proposal, request)
                .await?
                .with_deadline(deadline);
            let metadata = metadata_store_cloned(&state)?;
            if let Some(receipt) = metadata
                .ai_room_query_commit_receipt(prepared.clone())
                .await?
            {
                return Ok(AiRoomQueryProposalCommit {
                    proposal: prepared.committed_proposal(&receipt),
                    receipt,
                });
            }
            let rooms = state.rooms.clone();
            let owner = state.clone();
            let owned_headers = headers.clone();
            let bound = prepared.clone();
            let storage = metadata.clone();
            let (authored, receipt) = metadata_blocking(move || {
                let shared = rooms
                    .documents()
                    .get_or_load(&storage, bound.document_id())
                    .map_err(|_| {
                        ApiError::BadRequest("Room query document is unavailable".into())
                    })?;
                let mut native = shared.lock().map_err(|_| {
                    ApiError::Internal("Room query document owner is unavailable".into())
                })?;
                // A concurrent exact request may have committed while this one
                // waited for the actor. Observe its immutable proof before text.
                if let Some(receipt) = storage.ai_room_query_commit_receipt_now(&bound)? {
                    return Ok((None, receipt));
                }
                if owner.shutdown.is_draining() {
                    return Err(ApiError::ServiceDraining);
                }
                let fresh = resolve_auth_context(&owner, &owned_headers)?;
                super::ai_database::captured_review_caller(&owner, &fresh, &owned_headers)?;
                ensure_tenant(&fresh, TenantId(bound.tenant_id()))?;
                let authored = native
                    .author_ai_room_query_proposal(&storage, &bound)
                    .map_err(|_| {
                        ApiError::BadRequest(
                            "Room query text, permission or approval changed; review again".into(),
                        )
                    })?;
                let receipt = storage
                    .ai_room_query_commit_receipt_now(&bound)?
                    .ok_or_else(|| {
                        ApiError::Internal("Committed room query receipt is unavailable".into())
                    })?;
                Ok((Some(authored), receipt))
            })
            .await?;
            if let Some(authored) = authored {
                if let crate::document_actor::ApplyOutcome::Applied { server_seq, .. } =
                    authored.outcome
                {
                    state.rooms.publish_doc(
                        prepared.room_id(),
                        RoomServerMessage::DocumentUpdateCommitted {
                            document_id: prepared.document_id().0,
                            replica_id: ReplicaId(authored.replica_id),
                            server_seq,
                            update: CrdtUpdate::new(authored.update_bytes),
                            server_version: DocumentVersion::new(authored.server_version),
                        },
                    );
                    state.sessions.push_operation_full(
                        Operation::ApplyDocumentUpdate {
                            room_id: prepared.room_id(),
                            document_id: prepared.document_id().0,
                            update_id: proposal.to_string(),
                            server_seq,
                        },
                        OperationStatus::Succeeded,
                        Some(actor.0),
                        None,
                        None,
                        None,
                    );
                }
            }
            Ok(AiRoomQueryProposalCommit {
                proposal: prepared.committed_proposal(&receipt),
                receipt,
            })
        }),
    )
    .await?;
    // Delivery authorization never changes an already committed native receipt.
    let fresh =
        resolve_auth_context_blocking(delivery_state.clone(), delivery_headers.clone()).await?;
    super::ai_database::captured_review_caller(&delivery_state, &fresh, &delivery_headers)?;
    super::ai::check_ai_chat_scope(&delivery_state, &fresh, chat).await?;
    Ok(Json(result))
}

pub(super) async fn inspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((chat, proposal)): Path<(String, String)>,
) -> ApiResult<Json<AiRoomQueryProposalCommit>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    super::ai_database::captured_review_caller(&state, &auth, &headers)?;
    let chat = super::ai::ai_chat_id(&chat)?;
    let proposal = super::ai::ai_chat_id(&proposal)?;
    super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
    let result: ApiResult<_> = async {
        let metadata = metadata_store_cloned(&state)?;
        let detail = metadata
            .list_ai_query_proposals(chat, auth.principal_id)
            .await?
            .into_iter()
            .find(|detail| detail.proposal.id == proposal)
            .ok_or_else(|| ApiError::BadRequest("Room proposal is unavailable".into()))?;
        let prepared = prepare(
            &state,
            &auth,
            chat,
            proposal,
            CommitAiRoomQueryProposalRequest {
                expected_revision: detail
                    .proposal
                    .base_revision
                    .ok_or(ApiError::Unauthorized)?,
                expected_content_sha256: detail.proposal.content_sha256,
            },
        )
        .await?;
        let receipt = metadata
            .ai_room_query_commit_receipt(prepared.clone())
            .await?
            .ok_or_else(|| {
                ApiError::BadRequest(
                "No server room commit is recorded; an absent receipt does not authorize a retry"
                    .into(),
            )
            })?;
        let result = AiRoomQueryProposalCommit {
            proposal: prepared.committed_proposal(&receipt),
            receipt,
        };
        let fresh = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
        super::ai_database::captured_review_caller(&state, &fresh, &headers)?;
        super::ai::check_ai_chat_scope(&state, &fresh, chat).await?;
        Ok(result)
    }
    .await;
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "inspect_room_query_commit".into(),
            chat_id: Some(chat),
            run_id: None,
        },
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
