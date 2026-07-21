//! Room-domain operations shared by transport adapters.
//!
//! The WebSocket layer owns framing, leases, and presence. Document mutation
//! lives here so Phase G can replace the current snapshot-backed text model
//! without coupling CRDT semantics to axum.

use sift_doc::{CrdtKind, DocumentSnapshot, TextDocument, TextOperation};
use sift_metadata::{CrdtType, DocumentId, MetadataStore, PrincipalId, RoomId};

use crate::error::{ApiError, ApiResult};

pub async fn apply_document_operation(
    metadata: MetadataStore,
    principal: PrincipalId,
    room: RoomId,
    document: DocumentId,
    operation: sift_protocol::TextDocumentOperation,
) -> ApiResult<()> {
    tokio::task::spawn_blocking(move || {
        let row = metadata.get_document_for_principal(document, principal, true)?;
        if row.room_id != room {
            return Err(ApiError::Forbidden(format!(
                "document {:?} is not in room {:?}",
                document, room
            )));
        }
        let crdt = match row.crdt_type {
            CrdtType::Loro => CrdtKind::Loro,
            CrdtType::Automerge => CrdtKind::Automerge,
        };
        let mut doc = TextDocument::from_snapshot(DocumentSnapshot::new(crdt, row.crdt_state));
        let operation = match operation {
            sift_protocol::TextDocumentOperation::Replace { text } => {
                TextOperation::Replace { text }
            }
            sift_protocol::TextDocumentOperation::Insert { offset, text } => {
                TextOperation::Insert { offset, text }
            }
            sift_protocol::TextDocumentOperation::Delete { start, end } => {
                TextOperation::Delete { start, end }
            }
        };
        let snapshot = doc
            .apply(operation)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?;
        metadata.update_document_snapshot_for_principal(document, principal, snapshot.bytes)?;
        Ok(())
    })
    .await
    .map_err(|error| ApiError::Internal(format!("room document task failed: {error}")))?
}
