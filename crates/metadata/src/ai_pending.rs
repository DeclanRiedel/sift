//! Durable exact-request cancellation closes the create/response-loss race.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::CancelAiPendingTurnRequest;
use uuid::Uuid;

pub(crate) fn require_uncanceled(
    conn: &Connection,
    chat: Uuid,
    actor: PrincipalId,
    request: Uuid,
    desktop: Uuid,
) -> Result<()> {
    let canceled: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_pending_turn_cancel WHERE chat_id=?1 AND client_request_id=?2 AND desktop_id=?3 AND initiator_principal_id=?4)", params![chat.to_string(),request.to_string(),desktop.to_string(),actor.0], |row|row.get(0))?;
    if canceled {
        return Err(MetadataError::AiInvalid(
            "AI startup request was canceled; review context and send a fresh request".into(),
        ));
    }
    Ok(())
}
impl MetadataStore {
    /// No content or lease reads. An existing original request can close after
    /// access revocation; unknown requests require current chat access.
    pub async fn cancel_ai_pending_turn(
        &self,
        chat: Uuid,
        actor: PrincipalId,
        request: CancelAiPendingTurnRequest,
    ) -> Result<Option<Uuid>> {
        if request.client_request_id.is_nil() || request.desktop_id.is_nil() {
            return Err(MetadataError::AiInvalid(
                "Original non-nil startup identifiers are required".into(),
            ));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let run: Option<(String,i64,String,String)> = tx.query_row("SELECT id,initiator_principal_id,desktop_id,status FROM ai_run WHERE chat_id=?1 AND client_request_id=?2",params![chat.to_string(),request.client_request_id.to_string()], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
            if let Some((_, owner, desktop, _)) = &run {
                if *owner != actor.0 || *desktop != request.desktop_id.to_string() { return Err(MetadataError::AiAccessDenied); }
            } else { super::ai::require_chat_access(&tx,chat,actor)?; }
            let marked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_pending_turn_cancel WHERE chat_id=?1 AND client_request_id=?2 AND desktop_id=?3 AND initiator_principal_id=?4)",params![chat.to_string(),request.client_request_id.to_string(),request.desktop_id.to_string(),actor.0], |row|row.get(0))?;
            if !marked {
                let count: u64 = tx.query_row("SELECT COUNT(*) FROM ai_pending_turn_cancel WHERE chat_id=?1",[chat.to_string()], |row|row.get(0))?;
                if count < 1024 {
                    tx.execute("INSERT INTO ai_pending_turn_cancel(chat_id,client_request_id,desktop_id,initiator_principal_id,created_at) VALUES(?1,?2,?3,?4,?5)", params![chat.to_string(),request.client_request_id.to_string(),request.desktop_id.to_string(),actor.0,chrono::Utc::now().to_rfc3339()])?;
                } else if run.is_none() { return Err(MetadataError::AiInvalid("AI chat pending cancellation limit reached; use a new chat".into())); }
            }
            let id = if let Some((id,_,_,status)) = run {
                if status == "running" {
                    let now = chrono::Utc::now().to_rfc3339();
                    tx.execute("UPDATE ai_run SET status='canceled',ended_at=?2,next_sequence=next_sequence+1 WHERE id=?1",params![id,now])?;
                    tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at) SELECT id,next_sequence-1,'stopped',?2 FROM ai_run WHERE id=?1",params![id,now])?;
                }
                Some(Uuid::parse_str(&id).map_err(|_|MetadataError::AiContent("Invalid AI run identity".into()))?)
            } else { None };
            tx.commit()?;
            Ok(id)
        }).await
    }
}
